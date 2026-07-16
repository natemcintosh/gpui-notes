//! Application-wide color tokens.
//!
//! Keep visual decisions here so the dark theme can evolve without hunting
//! through view implementations. Callers use semantic names rather than raw
//! RGB values.

use gpui::{rgb, rgba, App, Rgba, SharedString};

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
    const CANDIDATES: [&str; 5] = [
        "Adwaita Sans",
        "Cantarell",
        "Noto Sans",
        "Ubuntu",
        "DejaVu Sans",
    ];
    CANDIDATES
        .into_iter()
        .find(|candidate| installed.iter().any(|name| name == candidate))
        .unwrap_or(".SystemUIFont")
        .into()
}

#[must_use]
pub fn window_bg() -> Rgba {
    rgb(0x1e1e1e)
}

#[must_use]
pub fn overlay_bg() -> Rgba {
    rgb(0x141414)
}

#[must_use]
pub fn overlay_border() -> Rgba {
    rgb(0x3a3a3a)
}

#[must_use]
pub fn fg() -> Rgba {
    rgb(0xe6e6e6)
}

#[must_use]
pub fn fg_muted() -> Rgba {
    rgb(0x9a9a9a)
}

#[must_use]
pub fn header_fg() -> Rgba {
    rgb(0xcccccc)
}

#[must_use]
pub fn chrome_fg() -> Rgba {
    rgb(0xaaaaaa)
}

#[must_use]
pub fn control_fg() -> Rgba {
    rgb(0x888888)
}

#[must_use]
pub fn control_hover_fg() -> Rgba {
    rgb(0xdddddd)
}

#[must_use]
pub fn overlay_title_fg() -> Rgba {
    rgb(0xeeeeee)
}

#[must_use]
pub fn empty_state_fg() -> Rgba {
    rgb(0x999999)
}

#[must_use]
pub fn bg_subtle() -> Rgba {
    rgb(0x2a2a2a)
}

#[must_use]
pub fn row_bg() -> Rgba {
    rgb(0x222222)
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
    rgb(0xffffff)
}

#[must_use]
pub fn accent() -> Rgba {
    rgb(0x66b2ff)
}

#[must_use]
pub fn code_bg() -> Rgba {
    window_bg()
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
    rgba(0x66b2_ff66)
}

#[must_use]
pub fn error_bg() -> Rgba {
    rgb(0x3a2222)
}

#[must_use]
pub fn error_hover_bg() -> Rgba {
    rgb(0x4a2a2a)
}

#[must_use]
pub fn error_fg() -> Rgba {
    rgb(0xff9999)
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
}
