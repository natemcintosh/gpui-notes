use chrono::{DateTime, Local};
use gpui::{App, BorrowAppContext, Global, SharedString};

/// The most recent I/O failure, surfaced by `RootView` as a dismissible line
/// under the header (#47). Action handlers set it via [`report`] where they
/// previously wrote to `eprintln!`, which is invisible when the app is
/// launched from a desktop entry. Holds only the latest error: a newer
/// failure replaces the old one.
#[derive(Default)]
pub struct LastError {
    current: Option<ErrorEntry>,
}

pub struct ErrorEntry {
    pub message: SharedString,
    pub at: DateTime<Local>,
}

impl Global for LastError {}

impl LastError {
    #[must_use]
    pub fn get(&self) -> Option<&ErrorEntry> {
        self.current.as_ref()
    }
}

/// Records `message` as the latest error. Goes through `update_global` so
/// `observe_global::<LastError>` observers re-render.
pub fn report(message: impl Into<SharedString>, cx: &mut App) {
    let message = message.into();
    cx.update_global::<LastError, ()>(|last, _| {
        last.current = Some(ErrorEntry {
            message,
            at: Local::now(),
        });
    });
}

/// Clears the error line.
pub fn dismiss(cx: &mut App) {
    cx.update_global::<LastError, ()>(|last, _| last.current = None);
}
