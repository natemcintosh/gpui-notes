#![allow(clippy::unreadable_literal)]

/// The platform's primary modifier for keybindings: `cmd` on macOS, `ctrl`
/// elsewhere. Every `bind_keys` site and user-facing shortcut label must go
/// through this so bindings and documentation stay in sync per platform.
#[must_use]
pub fn cmd_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    }
}

pub mod block_render;
pub mod block_view;
pub mod errors;
pub mod journal;
pub mod outline;
pub mod outline_view;
pub mod page;
pub mod page_picker;
pub mod registry;
pub mod store;
pub mod tags;
pub mod text_input;
pub mod theme;
pub mod window_frame;
