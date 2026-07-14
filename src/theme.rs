//! Application-wide color tokens.
//!
//! Keep visual decisions here so the dark theme can evolve without hunting
//! through view implementations. Callers use semantic names rather than raw
//! RGB values.

use gpui::{rgb, rgba, Rgba};

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
