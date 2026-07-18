#![allow(clippy::unreadable_literal)]

/// The platform's primary modifier for keybindings: `cmd` on macOS, `ctrl`
/// elsewhere. Every platform-specific `bind_keys` site goes through this;
/// shortcut UI renders the registered keystrokes themselves at runtime.
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
pub mod page_links;
pub mod page_picker;
pub mod registry;
pub mod shortcut_bar;
pub mod shortcut_hints;
pub mod store;
pub mod tags;
pub mod text_input;
pub mod theme;
pub mod window_frame;
