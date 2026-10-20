//! Application-wide color tokens.
//!
//! Keep visual decisions here so the themes can evolve without hunting
//! through view implementations. Callers use semantic names rather than raw
//! RGB values; each name reads from the active [`Theme`]'s palette, so
//! switching themes (`toggle`) changes every view on its next render.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use gpui::{px, rgba, App, Pixels, Rgba, SharedString, Window};

/// Widest the note column grows; wider windows centre it. Matches Logseq's
/// `--ls-main-content-max-width`.
pub const CONTENT_MAX_WIDTH: Pixels = px(810.);

/// The window's padding on each side of the content (`RootView`'s `p_4`).
const WINDOW_SIDE_INSET: Pixels = px(16.);

/// The note column's width in this window, in pixels.
///
/// Size the column with this, not `w_full` capped by `max_w` and centred
/// with `mx_auto`: some taffy passes measure a child without knowing a
/// percentage width, and with auto margins such a row shrinks to its text
/// and centres on its own — the text then paints from that measurement.
/// A definite width gives every pass the same answer.
#[must_use]
pub fn content_width(window: &Window) -> Pixels {
    (window.viewport_size().width - WINDOW_SIDE_INSET * 2.)
        .min(CONTENT_MAX_WIDTH)
        .max(px(0.))
}

/// The gap on each side of the note column inside the window's padding.
#[must_use]
pub fn content_gutter(window: &Window) -> Pixels {
    ((window.viewport_size().width - WINDOW_SIDE_INSET * 2. - content_width(window)) / 2.)
        .max(px(0.))
}

/// The app-wide UI font family.
///
/// GPUI's default `.SystemUIFont` maps to "IBM Plex Sans" on Linux — a font
/// Zed bundles as an asset but this app doesn't ship — and when a family
/// fails to resolve, gpui's fallback stack drops the requested weight/style,
/// so bold/italic silently render as regular text. Picking a family that is
/// actually installed lets the platform text system match its bold/italic
/// faces.
#[must_use]
pub fn ui_font_family(cx: &App) -> SharedString {
    if cfg!(target_os = "macos") {
        ".SystemUIFont".into()
    } else {
        pick_ui_font(&cx.text_system().all_font_names())
    }
}

/// Returns the first candidate family present in `installed`, falling back to
/// `.SystemUIFont` when none is (no worse than gpui's default behavior).
fn pick_ui_font(installed: &[String]) -> SharedString {
    const CANDIDATES: [&str; 6] = [
        "Adwaita Sans",
        "Cantarell",
        "Noto Sans",
        "Ubuntu",
        "DejaVu Sans",
        // Windows.
        "Segoe UI",
    ];
    first_installed(&CANDIDATES, installed).unwrap_or(".SystemUIFont".into())
}

/// The family for inline code and code blocks.
///
/// gpui hands the family name straight to the platform text system, which
/// does not resolve CSS generics like `"monospace"` — the lookup fails and
/// code silently renders in the proportional fallback font. Resolved once and
/// cached, since inline code asks for it on every render.
#[must_use]
pub fn mono_font_family(cx: &App) -> SharedString {
    static MONO: OnceLock<SharedString> = OnceLock::new();
    MONO.get_or_init(|| {
        if cfg!(target_os = "macos") {
            "Menlo".into()
        } else {
            pick_mono_font(&cx.text_system().all_font_names())
        }
    })
    .clone()
}

fn pick_mono_font(installed: &[String]) -> SharedString {
    const CANDIDATES: [&str; 8] = [
        "Adwaita Mono",
        "Noto Sans Mono",
        "DejaVu Sans Mono",
        "Ubuntu Mono",
        "Liberation Mono",
        "Source Code Pro",
        // Windows.
        "Cascadia Mono",
        "Consolas",
    ];
    first_installed(&CANDIDATES, installed).unwrap_or("monospace".into())
}

fn first_installed(candidates: &[&'static str], installed: &[String]) -> Option<SharedString> {
    candidates
        .iter()
        .find(|candidate| installed.iter().any(|name| name == *candidate))
        .map(|&name| name.into())
}

/// The two built-in colour themes, both from Ethan Schoonover's Solarized
/// palette. The app starts in [`Theme::SolarizedDark`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Theme {
    #[default]
    SolarizedDark,
    SolarizedLight,
}

impl Theme {
    /// The other theme.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            Self::SolarizedDark => Self::SolarizedLight,
            Self::SolarizedLight => Self::SolarizedDark,
        }
    }

    fn palette(self) -> &'static Palette {
        match self {
            Self::SolarizedDark => &SOLARIZED_DARK,
            Self::SolarizedLight => &SOLARIZED_LIGHT,
        }
    }
}

/// The active theme. A process-wide static rather than a gpui `Global`
/// because the colour functions below are called from ~100 render sites,
/// many without an `App` at hand. `false` is dark, `true` is light.
static LIGHT: AtomicBool = AtomicBool::new(false);

/// The theme every colour function currently answers for.
#[must_use]
pub fn current() -> Theme {
    if LIGHT.load(Ordering::Relaxed) {
        Theme::SolarizedLight
    } else {
        Theme::SolarizedDark
    }
}

/// Switch to `theme` and redraw every window, since nothing else notifies
/// the views that their colours changed.
pub fn set(theme: Theme, cx: &mut App) {
    LIGHT.store(theme == Theme::SolarizedLight, Ordering::Relaxed);
    cx.refresh_windows();
}

/// Switch between the dark and light themes.
pub fn toggle(cx: &mut App) {
    set(current().toggled(), cx);
}

/// The base colours each theme assigns; the role functions below derive
/// from these.
struct Palette {
    window_bg: Rgba,
    /// Overlays and menus.
    overlay_bg: Rgba,
    overlay_border: Rgba,
    /// List rows and inputs.
    row_bg: Rgba,
    /// Hover, chips, inline code and the client-side titlebar.
    bg_subtle: Rgba,
    /// Body text.
    fg: Rgba,
    /// Secondary text.
    fg_muted: Rgba,
    /// Titles, hovered controls and selected rows.
    fg_emphasis: Rgba,
    accent: Rgba,
    link_bracket: Rgba,
    error_bg: Rgba,
    error_hover_bg: Rgba,
    error_fg: Rgba,
    spell_underline: Rgba,
    /// Syntax-highlighted code: strings, other literals, keywords, types and
    /// function names. Comments use `fg_muted`.
    syntax_string: Rgba,
    syntax_constant: Rgba,
    syntax_keyword: Rgba,
    syntax_type: Rgba,
    syntax_function: Rgba,
}

/// `0xRRGGBB` as an opaque colour; `gpui::rgb` isn't `const`.
#[allow(clippy::cast_precision_loss)] // Each channel is at most 255.
const fn hex(rgb: u32) -> Rgba {
    Rgba {
        r: ((rgb >> 16) & 0xff) as f32 / 255.,
        g: ((rgb >> 8) & 0xff) as f32 / 255.,
        b: (rgb & 0xff) as f32 / 255.,
        a: 1.,
    }
}

// The sixteen Solarized colours (https://ethanschoonover.com/solarized/).
const BASE03: u32 = 0x002b36;
const BASE02: u32 = 0x073642;
const BASE01: u32 = 0x586e75;
const BASE00: u32 = 0x657b83;
const BASE0: u32 = 0x839496;
const BASE1: u32 = 0x93a1a1;
const BASE2: u32 = 0xeee8d5;
const BASE3: u32 = 0xfdf6e3;
const YELLOW: u32 = 0xb58900;
const RED: u32 = 0xdc322f;
const MAGENTA: u32 = 0xd33682;
const BLUE: u32 = 0x268bd2;
const CYAN: u32 = 0x2aa198;
const GREEN: u32 = 0x859900;

// Solarized has only two background tones per mode, and the UI needs a few
// more: the extra tones below are blends of neighbouring base colours (or of
// the background and red, for errors).
const SOLARIZED_DARK: Palette = Palette {
    window_bg: hex(BASE03),
    overlay_bg: hex(0x00212b),
    overlay_border: hex(0x1f4751),
    row_bg: hex(0x03313d),
    bg_subtle: hex(BASE02),
    fg: hex(BASE0),
    fg_muted: hex(BASE01),
    fg_emphasis: hex(BASE1),
    accent: hex(BLUE),
    link_bracket: hex(0x2f525b),
    error_bg: hex(0x422d34),
    error_hover_bg: hex(0x582e33),
    error_fg: hex(RED),
    spell_underline: hex(RED),
    syntax_string: hex(CYAN),
    syntax_constant: hex(MAGENTA),
    syntax_keyword: hex(GREEN),
    syntax_type: hex(YELLOW),
    syntax_function: hex(BLUE),
};

const SOLARIZED_LIGHT: Palette = Palette {
    window_bg: hex(BASE3),
    overlay_bg: hex(BASE2),
    overlay_border: hex(0xd3cbb7),
    row_bg: hex(0xf7f0dd),
    bg_subtle: hex(0xe4ddc8),
    fg: hex(BASE00),
    fg_muted: hex(BASE1),
    fg_emphasis: hex(BASE01),
    accent: hex(BLUE),
    link_bracket: hex(0xc0c4bb),
    error_bg: hex(0xf8d8c8),
    error_hover_bg: hex(0xf5c5b6),
    error_fg: hex(RED),
    spell_underline: hex(RED),
    syntax_string: hex(CYAN),
    syntax_constant: hex(MAGENTA),
    syntax_keyword: hex(GREEN),
    syntax_type: hex(YELLOW),
    syntax_function: hex(BLUE),
};

fn palette() -> &'static Palette {
    current().palette()
}

/// `color` at `alpha` (0–255) opacity.
fn with_alpha(color: Rgba, alpha: u8) -> Rgba {
    Rgba {
        a: f32::from(alpha) / 255.,
        ..color
    }
}

#[must_use]
pub fn window_bg() -> Rgba {
    palette().window_bg
}

#[must_use]
pub fn overlay_bg() -> Rgba {
    palette().overlay_bg
}

#[must_use]
pub fn overlay_border() -> Rgba {
    palette().overlay_border
}

#[must_use]
pub fn fg() -> Rgba {
    palette().fg
}

#[must_use]
pub fn fg_muted() -> Rgba {
    palette().fg_muted
}

#[must_use]
pub fn header_fg() -> Rgba {
    fg()
}

#[must_use]
pub fn chrome_fg() -> Rgba {
    fg()
}

#[must_use]
pub fn control_fg() -> Rgba {
    fg_muted()
}

#[must_use]
pub fn control_hover_fg() -> Rgba {
    palette().fg_emphasis
}

#[must_use]
pub fn overlay_title_fg() -> Rgba {
    palette().fg_emphasis
}

#[must_use]
pub fn empty_state_fg() -> Rgba {
    fg_muted()
}

#[must_use]
pub fn bg_subtle() -> Rgba {
    palette().bg_subtle
}

#[must_use]
pub fn row_bg() -> Rgba {
    palette().row_bg
}

#[must_use]
pub fn row_hover() -> Rgba {
    bg_subtle()
}

#[must_use]
pub fn row_selected() -> Rgba {
    overlay_border()
}

#[must_use]
pub fn row_selected_fg() -> Rgba {
    palette().fg_emphasis
}

#[must_use]
pub fn accent() -> Rgba {
    palette().accent
}

/// The `[[`/`]]` around a page link: dimmer than `fg_muted`, like Logseq.
#[must_use]
pub fn link_bracket() -> Rgba {
    palette().link_bracket
}

/// The ring behind a folded block's bullet.
#[must_use]
pub fn folded_bullet_ring() -> Rgba {
    overlay_border()
}

/// The faint line down each parent's bullet column, beside its children.
#[must_use]
pub fn thread_guide() -> Rgba {
    overlay_border()
}

#[must_use]
pub fn heading_rule() -> Rgba {
    overlay_border()
}

#[must_use]
pub fn code_bg() -> Rgba {
    window_bg()
}

/// Behind a ```` ```calc ```` block: a tint off the page, so it reads as a
/// live panel rather than plain code.
#[must_use]
pub fn calc_bg() -> Rgba {
    row_bg()
}

/// A calc block's outline and the hairline before its result column.
#[must_use]
pub fn calc_border() -> Rgba {
    overlay_border()
}

#[must_use]
pub fn syntax_string() -> Rgba {
    palette().syntax_string
}

#[must_use]
pub fn syntax_constant() -> Rgba {
    palette().syntax_constant
}

#[must_use]
pub fn syntax_keyword() -> Rgba {
    palette().syntax_keyword
}

#[must_use]
pub fn syntax_type() -> Rgba {
    palette().syntax_type
}

#[must_use]
pub fn syntax_function() -> Rgba {
    palette().syntax_function
}

#[must_use]
pub fn input_outer_bg() -> Rgba {
    bg_subtle()
}

#[must_use]
pub fn input_bg() -> Rgba {
    row_bg()
}

#[must_use]
pub fn input_fg() -> Rgba {
    fg()
}

#[must_use]
pub fn input_placeholder_fg() -> Rgba {
    fg_muted()
}

#[must_use]
pub fn input_cursor() -> Rgba {
    accent()
}

#[must_use]
pub fn input_selection() -> Rgba {
    with_alpha(accent(), 0x66)
}

/// Behind whole blocks selected by dragging across rows.
#[must_use]
pub fn block_selection() -> Rgba {
    with_alpha(accent(), 0x33)
}

/// Behind the matched text in a search result snippet.
#[must_use]
pub fn search_match_bg() -> Rgba {
    with_alpha(accent(), 0x55)
}

#[must_use]
pub fn error_bg() -> Rgba {
    palette().error_bg
}

#[must_use]
pub fn error_hover_bg() -> Rgba {
    palette().error_hover_bg
}

#[must_use]
pub fn error_fg() -> Rgba {
    palette().error_fg
}

/// The wavy underline under a misspelled word.
#[must_use]
pub fn spell_underline() -> Rgba {
    palette().spell_underline
}

#[must_use]
pub fn transparent() -> Rgba {
    rgba(0x0000_0000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_first_installed_candidate() {
        let installed = vec!["DejaVu Sans".to_string(), "Cantarell".to_string()];
        assert_eq!(pick_ui_font(&installed).as_ref(), "Cantarell");
    }

    #[test]
    fn falls_back_to_system_ui_font_when_no_candidate_installed() {
        let installed = vec!["Comic Sans MS".to_string()];
        assert_eq!(pick_ui_font(&installed).as_ref(), ".SystemUIFont");
    }

    #[test]
    fn picks_installed_mono_font_over_generic() {
        let installed = vec!["Liberation Mono".to_string(), "Noto Sans".to_string()];
        assert_eq!(pick_mono_font(&installed).as_ref(), "Liberation Mono");
    }
}
