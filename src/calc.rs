//! Logseq-style `calc` blocks: every line of a ```` ```calc ```` fence is an
//! arithmetic expression, evaluated top to bottom in one shared scope. Results
//! are display-only — the markdown on disk stays exactly what the user typed.

use std::collections::HashMap;
use std::fmt::Write as _;

use gpui::{div, AnyElement, App, IntoElement, ParentElement, Styled, Window};

use crate::theme;

/// What one source line evaluated to. Indices line up with the block's lines.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Blank line — spacing, nothing to show.
    Blank,
    Value(f64),
    Error(String),
}

/// Evaluate every line of a calc block body in a shared scope.
#[must_use]
pub fn eval_block(body: &str) -> Vec<Outcome> {
    let mut scope = Scope::default();
    body.lines().map(|line| scope.eval_line(line)).collect()
}

/// Render a `f64` for display: full precision up to six decimals, with the
/// trailing zeros (and float noise like `0.30000000000000004`) trimmed off.
#[must_use]
pub fn format_number(n: f64) -> String {
    let mut s = format!("{n:.6}");
    if s.contains('.') {
        s.truncate(s.trim_end_matches('0').trim_end_matches('.').len());
    }
    s
}

// --- Evaluation -----------------------------------------------------------

#[derive(Default)]
struct Scope {
    vars: HashMap<String, f64>,
    /// Every preceding line that produced a value, for `sum`/`total`/`avg`.
    results: Vec<f64>,
}

impl Scope {
    fn eval_line(&mut self, line: &str) -> Outcome {
        let line = line.trim();
        if line.is_empty() {
            return Outcome::Blank;
        }
        let (name, expr) = split_assignment(line);
        match eval_expr(expr, self) {
            Ok(n) => {
                if let Some(name) = name {
                    self.vars.insert(name.to_string(), n);
                }
                self.results.push(n);
                Outcome::Value(n)
            }
            Err(e) => Outcome::Error(e),
        }
    }

    fn lookup(&self, name: &str) -> Option<f64> {
        match name {
            "sum" | "total" => Some(self.results.iter().sum()),
            #[allow(clippy::cast_precision_loss)]
            "avg" => (!self.results.is_empty())
                .then(|| self.results.iter().sum::<f64>() / self.results.len() as f64),
            "last" => self.results.last().copied(),
            _ => self.vars.get(name).copied(),
        }
    }
}

/// Splits `name = expr` into its parts. A leading `name =` only counts when
/// `name` is a bare identifier, so `1 + 2 = 3` stays a (failing) expression.
fn split_assignment(line: &str) -> (Option<&str>, &str) {
    let Some((lhs, rhs)) = line.split_once('=') else {
        return (None, line);
    };
    let name = lhs.trim();
    if !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        (Some(name), rhs)
    } else {
        (None, line)
    }
}

/// A number that may still be a bare percentage. `10%` keeps `n == 10.0` so
/// that `250 + 10%` can read it as "10 percent *of 250*"; everywhere else it
/// collapses to `n / 100`.
#[derive(Clone, Copy)]
struct Val {
    n: f64,
    pct: bool,
}

impl Val {
    fn plain(self) -> f64 {
        if self.pct {
            self.n / 100.0
        } else {
            self.n
        }
    }
}

fn number(n: f64) -> Val {
    Val { n, pct: false }
}

fn eval_expr(input: &str, scope: &Scope) -> Result<f64, String> {
    let tokens = tokenize(input)?;
    let mut p = Parser {
        tokens: &tokens,
        pos: 0,
        scope,
    };
    let value = p.expr()?;
    if p.pos < p.tokens.len() {
        return Err(format!("unexpected `{}`", p.tokens[p.pos]));
    }
    let n = value.plain();
    if n.is_finite() {
        Ok(n)
    } else if n.is_nan() {
        Err("not a number".into())
    } else {
        Err("division by zero".into())
    }
}

// --- Tokens ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    /// Postfix `%`, as distinct from the `%` remainder operator.
    Percent,
    Sym(char),
}

impl std::fmt::Display for Tok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tok::Num(n) => f.write_str(&format_number(*n)),
            Tok::Ident(s) => f.write_str(s),
            Tok::Percent => f.write_char('%'),
            Tok::Sym(c) => f.write_char(*c),
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            c if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let n = text.parse().map_err(|_| format!("bad number `{text}`"))?;
                out.push(Tok::Num(n));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            '%' => {
                i += 1;
                // ponytail: `%` is postfix-percent when nothing can follow it as
                // an operand, and remainder otherwise. Good enough for `250 +
                // 10%` vs `10 % 3`; a real fix needs percent in the grammar.
                let rest: String = chars[i..].iter().collect();
                let rest = rest.trim_start();
                let is_percent = rest.is_empty()
                    || rest.starts_with(|c| "+-*/^),%".contains(c))
                    || rest
                        .strip_prefix("of")
                        .is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace));
                out.push(if is_percent {
                    Tok::Percent
                } else {
                    Tok::Sym('%')
                });
            }
            '+' | '-' | '*' | '/' | '^' | '(' | ')' | ',' => {
                out.push(Tok::Sym(c));
                i += 1;
            }
            _ => return Err(format!("unexpected `{c}`")),
        }
    }
    Ok(out)
}

// --- Parser (recursive descent, evaluating as it goes) --------------------

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    scope: &'a Scope,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn eat_sym(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Sym(c)) {
            self.pos += 1;
            return true;
        }
        false
    }

    /// `term (('+' | '-') term)*`
    fn expr(&mut self) -> Result<Val, String> {
        let mut left = self.term()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Sym(c @ ('+' | '-'))) => *c,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.term()?;
            // `250 + 10%` means 250 plus a tenth *of 250*, not plus 0.1.
            let delta = if right.pct && !left.pct {
                left.n * right.n / 100.0
            } else {
                right.plain()
            };
            left = number(if op == '+' {
                left.plain() + delta
            } else {
                left.plain() - delta
            });
        }
    }

    /// `unary (('*' | '/' | '%' | 'of') unary)*`
    fn term(&mut self) -> Result<Val, String> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Sym(c @ ('*' | '/' | '%'))) => *c,
                // `10% of 250`: `of` is just multiplication that reads better.
                Some(Tok::Ident(name)) if name == "of" => 'o',
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.unary()?;
            let (l, r) = (left.plain(), right.plain());
            left = number(match op {
                '*' | 'o' => l * r,
                '/' => {
                    if r == 0.0 {
                        return Err("division by zero".into());
                    }
                    l / r
                }
                _ => {
                    if r == 0.0 {
                        return Err("division by zero".into());
                    }
                    l % r
                }
            });
        }
    }

    /// `'-' unary | power`. Binds looser than `^`, so `-2^2` is `-4`.
    fn unary(&mut self) -> Result<Val, String> {
        if self.eat_sym('-') {
            return Ok(number(-self.unary()?.plain()));
        }
        self.eat_sym('+');
        self.power()
    }

    /// `atom ('^' unary)?` — right-associative.
    fn power(&mut self) -> Result<Val, String> {
        let base = self.atom()?;
        if self.eat_sym('^') {
            let exp = self.unary()?;
            return Ok(number(base.plain().powf(exp.plain())));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<Val, String> {
        let mut value = match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                number(n)
            }
            Some(Tok::Sym('(')) => {
                self.pos += 1;
                let inner = self.expr()?;
                if !self.eat_sym(')') {
                    return Err("unbalanced parens".into());
                }
                inner
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                if self.eat_sym('(') {
                    let args = self.args()?;
                    number(call(&name, &args)?)
                } else {
                    number(
                        self.scope
                            .lookup(&name)
                            .ok_or_else(|| format!("unknown name `{name}`"))?,
                    )
                }
            }
            Some(tok) => return Err(format!("unexpected `{tok}`")),
            None => return Err("unexpected end of line".into()),
        };
        if self.peek() == Some(&Tok::Percent) {
            self.pos += 1;
            value = Val {
                n: value.plain(),
                pct: true,
            };
        }
        Ok(value)
    }

    fn args(&mut self) -> Result<Vec<f64>, String> {
        let mut args = Vec::new();
        if self.eat_sym(')') {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?.plain());
            if self.eat_sym(')') {
                return Ok(args);
            }
            if !self.eat_sym(',') {
                return Err("unbalanced parens".into());
            }
        }
    }
}

fn call(name: &str, args: &[f64]) -> Result<f64, String> {
    let one = |f: fn(f64) -> f64| match args {
        [x] => Ok(f(*x)),
        _ => Err(format!("`{name}` takes 1 argument")),
    };
    match name {
        "sqrt" => one(f64::sqrt),
        "ln" => one(f64::ln),
        "log" => one(f64::log10),
        "abs" => one(f64::abs),
        "round" => one(f64::round),
        "floor" => one(f64::floor),
        "ceil" => one(f64::ceil),
        "min" | "max" => {
            let mut it = args.iter().copied();
            let first = it
                .next()
                .ok_or_else(|| format!("`{name}` needs arguments"))?;
            Ok(it.fold(first, if name == "min" { f64::min } else { f64::max }))
        }
        _ => Err(format!("unknown function `{name}`")),
    }
}

// --- Rendering ------------------------------------------------------------

/// Renders a calc block: each source line on the left, its result on the right.
//
// ponytail: re-evaluated every render; memoize on block text if a large page
// ever stutters.
pub fn render(body: &str, _window: &mut Window, _cx: &mut App) -> AnyElement {
    let mut root = div()
        .bg(theme::code_bg())
        .font_family("monospace")
        .p_2()
        .rounded_sm()
        .flex()
        .flex_col();
    for (line, outcome) in body.lines().zip(eval_block(body)) {
        let (result, color) = match outcome {
            Outcome::Blank => (String::new(), theme::fg_muted()),
            Outcome::Value(n) => (format_number(n), theme::accent()),
            Outcome::Error(e) => (e, theme::error_fg()),
        };
        root = root.child(
            div()
                .flex()
                .justify_between()
                .gap_4()
                .child(div().text_color(theme::fg()).child(line.to_string()))
                .child(div().text_color(color).child(result)),
        );
    }
    root.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn eval(src: &str) -> Result<f64, String> {
        eval_expr(src, &Scope::default())
    }

    /// What the renderer would put beside the line — float noise already gone.
    fn display(outcome: &Outcome) -> String {
        match outcome {
            Outcome::Blank => String::new(),
            Outcome::Value(n) => format_number(*n),
            Outcome::Error(e) => format!("error: {e}"),
        }
    }

    #[rstest]
    #[case("1 + 2", 3.0)]
    #[case("2 + 3 * 4", 14.0)]
    #[case("(2 + 3) * 4", 20.0)]
    #[case("10 - 2 - 3", 5.0)]
    #[case("100 / 4 / 5", 5.0)]
    #[case("10 % 3", 1.0)]
    #[case("2 ^ 3 ^ 2", 512.0)]
    #[case("-2 ^ 2", -4.0)]
    #[case("-(3 + 4)", -7.0)]
    #[case("- -5", 5.0)]
    #[case("2 * -3", -6.0)]
    #[case(".5 + .25", 0.75)]
    #[case("10% of 250", 25.0)]
    #[case("250 + 10%", 275.0)]
    #[case("250 - 10%", 225.0)]
    #[case("50%", 0.5)]
    #[case("sqrt(16)", 4.0)]
    #[case("ln(1)", 0.0)]
    #[case("log(1000)", 3.0)]
    #[case("abs(0 - 7)", 7.0)]
    #[case("round(2.5)", 3.0)]
    #[case("floor(2.9)", 2.0)]
    #[case("ceil(2.1)", 3.0)]
    #[case("min(3, 1, 2)", 1.0)]
    #[case("max(3, 1, 2)", 3.0)]
    #[case("sqrt(9) * max(2, 1)", 6.0)]
    fn evaluates(#[case] src: &str, #[case] expected: f64) {
        let got = eval(src).unwrap_or_else(|e| panic!("{src:?} failed: {e}"));
        assert!(
            (got - expected).abs() < 1e-9,
            "{src:?} = {got}, want {expected}"
        );
    }

    #[rstest]
    #[case("1 / 0", "division by zero")]
    #[case("1 % 0", "division by zero")]
    #[case("nope", "unknown name `nope`")]
    #[case("(1 + 2", "unbalanced parens")]
    #[case("1 +", "unexpected end of line")]
    #[case("1 2", "unexpected `2`")]
    #[case("sqrt(1, 2)", "`sqrt` takes 1 argument")]
    #[case("nope(1)", "unknown function `nope`")]
    #[case("1 $ 2", "unexpected `$`")]
    fn reports(#[case] src: &str, #[case] expected: &str) {
        assert_eq!(eval(src).unwrap_err(), expected);
    }

    #[test]
    fn variables_bind_and_carry_forward() {
        let out = eval_block("groceries = 12.50 + 8.99\ntip = groceries * 0.2\ngroceries + tip");
        let shown: Vec<String> = out.iter().map(display).collect();
        assert_eq!(shown, ["21.49", "4.298", "25.788"]);
    }

    #[test]
    fn sum_total_avg_and_last_see_preceding_results() {
        // Each aggregate is itself a result line, so it feeds the next one:
        // sum = 1+2+3, total = 1+2+3+6, avg = (1+2+3+6+12)/5, last = avg.
        let shown: Vec<String> = eval_block("1\n2\n3\nsum\ntotal\navg\nlast")
            .iter()
            .map(display)
            .collect();
        assert_eq!(shown, ["1", "2", "3", "6", "12", "4.8", "4.8"]);
    }

    #[test]
    fn aggregates_of_nothing_are_unknown_names() {
        assert_eq!(
            eval_block("avg\nlast"),
            vec![
                Outcome::Error("unknown name `avg`".into()),
                Outcome::Error("unknown name `last`".into()),
            ]
        );
    }

    #[test]
    fn a_failing_line_does_not_abort_the_block() {
        let out = eval_block("x = 2\nnope +\nx * 3");
        assert_eq!(out[0], Outcome::Value(2.0));
        assert!(matches!(out[1], Outcome::Error(_)));
        assert_eq!(out[2], Outcome::Value(6.0));
    }

    #[test]
    fn blank_lines_pass_through_and_do_not_shift_results() {
        let out = eval_block("1\n\n   \n2\nsum");
        assert_eq!(
            out,
            vec![
                Outcome::Value(1.0),
                Outcome::Blank,
                Outcome::Blank,
                Outcome::Value(2.0),
                Outcome::Value(3.0),
            ]
        );
    }

    #[test]
    fn assignment_shows_its_own_value_and_rebinds() {
        let out = eval_block("x = 1\nx = x + 5\nx");
        assert_eq!(
            out,
            vec![
                Outcome::Value(1.0),
                Outcome::Value(6.0),
                Outcome::Value(6.0)
            ]
        );
    }

    #[test]
    fn a_non_identifier_left_side_is_not_an_assignment() {
        assert!(matches!(eval_block("1 + 2 = 3")[0], Outcome::Error(_)));
    }

    /// The whole chain the renderer depends on: markdown on disk → outline
    /// block text → a `calc`-tagged code block → results.
    #[test]
    fn a_calc_fence_stored_in_an_outline_reaches_the_evaluator() {
        use crate::block_render::{lower, BlockNode};
        use crate::outline::Outline;

        let outline = Outline::parse(
            "- Trip\n  - ```calc\n    flights = 420\n    hotel = 3 * 180\n    total\n    ```\n",
        );
        let blocks = lower(&outline.roots[0].children[0].text, &[]);
        let [BlockNode::CodeBlock { lang, text }] = &blocks[..] else {
            panic!("expected one code block, got {blocks:?}");
        };
        assert_eq!(lang.as_ref().map(AsRef::as_ref), Some("calc"));

        let shown: Vec<String> = eval_block(text).iter().map(display).collect();
        assert_eq!(shown, ["420", "540", "960"]);
    }

    #[rstest]
    #[case(3.0, "3")]
    #[case(0.1 + 0.2, "0.3")]
    #[case(21.49, "21.49")]
    #[case(-2.5, "-2.5")]
    #[case(1.0 / 3.0, "0.333333")]
    fn formats_numbers(#[case] n: f64, #[case] expected: &str) {
        assert_eq!(format_number(n), expected);
    }
}
