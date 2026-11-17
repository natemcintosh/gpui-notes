#![allow(clippy::unreadable_literal)]

/// The platform's primary modifier for keybindings: `cmd` on macOS, `ctrl`
/// elsewhere. Every platform-specific `bind_keys` site goes through this, via
/// [`cmd!`]; shortcut UI renders the registered keystrokes themselves at
/// runtime.
#[must_use]
pub fn cmd_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    }
}

/// [`cmd_key`] joined to the rest of a keystroke at compile time:
/// `cmd!("shift-z")` is `"cmd-shift-z"` on macOS and `"ctrl-shift-z"`
/// elsewhere. `KeyBinding::new` takes a `&'static str`, so bindings can't
/// use a runtime `format!`.
#[macro_export]
macro_rules! cmd {
    ($keys:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $keys)
        } else {
            concat!("ctrl-", $keys)
        }
    };
}

pub mod assets;
pub mod autoscroll;
pub mod block_render;
pub mod block_view;
pub mod calc;
pub mod config;
pub mod errors;
pub mod goto_word;
pub mod history;
pub mod journal;
pub mod journals_view;
pub mod linked_refs;
pub mod outline;
pub mod outline_view;
pub mod page;
pub mod page_completion;
pub mod page_links;
pub mod page_picker;
pub mod registry;
pub mod search;
pub mod shortcut_hints;
pub mod shortcut_overlay;
pub mod source;
pub mod spellcheck;
pub mod store;
pub mod syntax;
pub mod text_input;
pub mod theme;
pub mod window_frame;
