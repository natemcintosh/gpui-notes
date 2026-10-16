//! Keeps what the keyboard is working on inside the visible part of the
//! nearest scroll container: the caret while typing, and the focused or
//! selected row after an arrow key.
//!
//! The rect is only known once the frame is laid out, and the element that
//! knows it can't name the scroll container (a journal day's outline is
//! scrolled by `JournalsView`, a linked reference by its page's
//! `OutlineView`). So `request` dispatches `RevealBounds` from the focused
//! element after the frame, and whichever scroll container is the nearest
//! ancestor handles it with `reveal`.

use gpui::{point, px, App, Bounds, Global, Pixels, ScrollHandle, Window};

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = gpui_notes, no_json)]
pub struct RevealBounds {
    /// Window coordinates, as laid out in the frame that requested it.
    pub bounds: Bounds<Pixels>,
}

/// The latest rect waiting for its `RevealBounds` to be dispatched.
#[derive(Default)]
struct Pending(Option<Bounds<Pixels>>);
impl Global for Pending {}

/// Ask the nearest scroll container around the focused element to bring
/// `bounds` into view, once the current frame is done.
///
/// Requests coalesce, latest wins: `reveal` scrolls by how far the rect sat
/// outside the viewport, so applying a stale rect after a newer one (several
/// frames can paint before the task runs, e.g. on key repeat) would scroll
/// past the target.
pub fn request(bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let already_queued = cx.default_global::<Pending>().0.replace(bounds).is_some();
    if already_queued {
        return;
    }
    // Called mid-draw, where nothing can schedule a redraw; the spawned task
    // runs once this frame is done.
    window
        .spawn(cx, async move |cx| {
            cx.update(|window, cx| {
                if let Some(bounds) = cx.global_mut::<Pending>().0.take() {
                    window.dispatch_action(Box::new(RevealBounds { bounds }), cx);
                }
            })
            .ok();
        })
        .detach();
}

/// Room left around a revealed rect, enough for a block's padding and border
/// below its caret line.
const MARGIN: Pixels = px(4.);

/// Scroll `handle` the least distance that shows `bounds` (plus `MARGIN`), top
/// first if it is taller than the viewport. Returns whether the offset
/// changed, in which case the owner must re-render.
#[must_use]
pub fn reveal(handle: &ScrollHandle, bounds: Bounds<Pixels>) -> bool {
    let bounds = bounds.dilate(MARGIN);
    let viewport = handle.bounds();
    let offset = handle.offset();
    let y = if bounds.top() < viewport.top() || bounds.size.height > viewport.size.height {
        offset.y + (viewport.top() - bounds.top())
    } else if bounds.bottom() > viewport.bottom() {
        offset.y - (bounds.bottom() - viewport.bottom())
    } else {
        return false;
    };
    let y = y.clamp(-handle.max_offset().y, Pixels::ZERO);
    if y == offset.y {
        return false;
    }
    handle.set_offset(point(offset.x, y));
    true
}
