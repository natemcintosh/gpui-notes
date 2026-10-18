//! Syntax highlighting for fenced code blocks in rendered blocks.
//!
//! Grammars are bat's Sublime syntaxes (via `two-face`), parsed by `syntect`.
//! Each token is reduced to a [`Class`], and the class is coloured from the
//! theme only when the block is drawn, so a theme switch recolours code
//! without highlighting it again.
//!
//! `render` runs on every keystroke, so results are cached by language and
//! source text.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui::Rgba;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::theme;

/// The kinds of token the theme colours differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Plain,
    Comment,
    String,
    /// Numbers, `true`/`None`-style language constants, escapes.
    Constant,
    Keyword,
    Type,
    Function,
}

impl Class {
    #[must_use]
    pub fn color(self) -> Rgba {
        match self {
            Self::Plain => theme::fg(),
            Self::Comment => theme::fg_muted(),
            Self::String => theme::syntax_string(),
            Self::Constant => theme::syntax_constant(),
            Self::Keyword => theme::syntax_keyword(),
            Self::Type => theme::syntax_type(),
            Self::Function => theme::syntax_function(),
        }
    }
}

/// Byte ranges of the source, each with its class. Adjacent ranges of the
/// same class are merged, and together they cover the whole text.
pub type Spans = Rc<[(Range<usize>, Class)]>;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Scope prefixes, most specific first: `storage.type` must beat `storage`.
static CLASSES: LazyLock<Vec<(Scope, Class)>> = LazyLock::new(|| {
    [
        ("comment", Class::Comment),
        ("string", Class::String),
        ("constant", Class::Constant),
        ("storage.type", Class::Type),
        ("support.type", Class::Type),
        ("support.class", Class::Type),
        ("entity.name.type", Class::Type),
        ("entity.name.class", Class::Type),
        ("entity.name.function", Class::Function),
        ("support.function", Class::Function),
        ("variable.function", Class::Function),
        ("keyword", Class::Keyword),
        ("storage", Class::Keyword),
    ]
    .into_iter()
    .map(|(prefix, class)| (Scope::new(prefix).expect("valid scope"), class))
    .collect()
});

thread_local! {
    /// Keyed by source text; the value remembers which language it was
    /// highlighted as.
    static CACHE: RefCell<HashMap<String, (String, Option<Spans>)>> =
        RefCell::new(HashMap::new());
}

/// Bounds the cache; past this it is simply emptied.
const CACHE_LIMIT: usize = 512;

/// Highlight `code` as the language named by a fence's info string
/// (`python`, `SQL`, `rust ignore`). `None` when the language is missing or
/// unknown, so the caller renders the code plain.
#[must_use]
pub fn highlight(info: &str, code: &str) -> Option<Spans> {
    CACHE.with(|cache| {
        if let Some((lang, spans)) = cache.borrow().get(code) {
            if lang == info {
                return spans.clone();
            }
        }
        let spans = compute(info, code);
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(code.to_owned(), (info.to_owned(), spans.clone()));
        spans
    })
}

fn compute(info: &str, code: &str) -> Option<Spans> {
    let token = info.split_whitespace().next()?.to_ascii_lowercase();
    let token = if token == "shell" { "sh" } else { &token };
    let syntax = SYNTAXES.find_syntax_by_token(token)?;

    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut spans: Vec<(Range<usize>, Class)> = Vec::new();
    let mut push = |range: Range<usize>, class: Class| {
        if range.is_empty() {
            return;
        }
        match spans.last_mut() {
            Some((last, last_class)) if *last_class == class && last.end == range.start => {
                last.end = range.end;
            }
            _ => spans.push((range, class)),
        }
    };

    let mut line_start = 0;
    for line in LinesWithEndings::from(code) {
        let ops = state.parse_line(line, &SYNTAXES).ok()?;
        let mut pos = 0;
        for (ix, op) in ops {
            push(line_start + pos..line_start + ix, classify(&stack));
            stack.apply(&op).ok()?;
            pos = ix;
        }
        push(line_start + pos..line_start + line.len(), classify(&stack));
        line_start += line.len();
    }
    Some(spans.into())
}

/// The class of the innermost scope that has one: punctuation inside a
/// string is coloured as the string.
fn classify(stack: &ScopeStack) -> Class {
    stack
        .as_slice()
        .iter()
        .rev()
        .find_map(|scope| {
            CLASSES
                .iter()
                .find(|(prefix, _)| prefix.is_prefix_of(*scope))
                .map(|&(_, class)| class)
        })
        .unwrap_or(Class::Plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn class_of(spans: &[(Range<usize>, Class)], code: &str, needle: &str) -> Class {
        let at = code.find(needle).expect("needle in code");
        spans
            .iter()
            .find(|(range, _)| range.contains(&at))
            .map(|&(_, class)| class)
            .expect("spans cover the text")
    }

    #[test]
    fn python_tokens_are_classified() {
        let code = "def f(x):\n    # note\n    return \"hi\" + 42\n";
        let spans = highlight("python", code).expect("python is known");
        assert_eq!(class_of(&spans, code, "def"), Class::Keyword);
        assert_eq!(class_of(&spans, code, "f("), Class::Function);
        assert_eq!(class_of(&spans, code, "# note"), Class::Comment);
        assert_eq!(class_of(&spans, code, "\"hi\""), Class::String);
        assert_eq!(class_of(&spans, code, "42"), Class::Constant);
    }

    #[test]
    fn spans_cover_the_whole_text_in_order() {
        let code = "fn main() {\n    let x = 1;\n}\n";
        let spans = highlight("rust", code).expect("rust is known");
        let mut end = 0;
        for (range, _) in spans.iter() {
            assert_eq!(range.start, end);
            end = range.end;
        }
        assert_eq!(end, code.len());
    }

    /// The fence spellings used in the author's graph.
    #[rstest]
    #[case("python")]
    #[case("SQL")]
    #[case("julia")]
    #[case("R")]
    #[case("sh")]
    #[case("shell")]
    #[case("bash")]
    #[case("rust")]
    #[case("go")]
    #[case("toml")]
    #[case("yaml")]
    #[case("json")]
    #[case("html")]
    #[case("python {:height 120}")]
    fn known_languages_resolve(#[case] info: &str) {
        assert!(highlight(info, "x\n").is_some(), "{info} should resolve");
    }

    #[rstest]
    #[case("")]
    #[case("not-a-language")]
    fn unknown_languages_fall_back(#[case] info: &str) {
        assert!(highlight(info, "x\n").is_none());
    }
}
