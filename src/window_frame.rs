//! Reusable wrapper view that draws minimal client-side window chrome (title
//! bar + 8 resize regions) when the compositor doesn't supply server-side
//! decorations.
//!
//! On compositors that honor `zxdg_decoration` (KDE, Sway, Hyprland, X11) GPUI
//! reports `Decorations::Server` and the wrapper passes the child through
//! unchanged. On GNOME/Mutter Wayland it falls back to `Decorations::Client`
//! and we draw our own chrome. Pattern adapted from zed's
//! `crates/gpui/examples/window_shadow.rs` at the pinned rev.

use gpui::{
    canvas, div, point, prelude::*, px, rgb, Bounds, Context, CursorStyle, Decorations, Entity,
    HitboxBehavior, IntoElement, MouseButton, ParentElement, Pixels, Point, Render, ResizeEdge,
    SharedString, Size, Styled, Window,
};

/// Thickness of the resize hit zone along each edge.
const EDGE: Pixels = px(6.0);
const TITLEBAR_HEIGHT: Pixels = px(28.0);

pub struct WindowFrame<V: Render + 'static> {
    title: SharedString,
    child: Entity<V>,
}

impl<V: Render + 'static> WindowFrame<V> {
    pub fn new(title: impl Into<SharedString>, child: Entity<V>) -> Self {
        Self {
            title: title.into(),
            child,
        }
    }
}

impl<V: Render + 'static> Render for WindowFrame<V> {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let decorations = window.window_decorations();
        let is_client = matches!(decorations, Decorations::Client { .. });
        let title = self.title.clone();
        let child = self.child.clone();

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .when(is_client, |d| {
                d.child(
                    canvas(
                        |_bounds, window, _cx| {
                            window.insert_hitbox(
                                Bounds::new(
                                    point(px(0.0), px(0.0)),
                                    window.window_bounds().get_bounds().size,
                                ),
                                HitboxBehavior::Normal,
                            )
                        },
                        |_bounds, hitbox, window, _cx| {
                            let pos = window.mouse_position();
                            let size = window.window_bounds().get_bounds().size;
                            if let Some(edge) = resize_edge(pos, EDGE, size) {
                                window.set_cursor_style(cursor_for(edge), &hitbox);
                            }
                        },
                    )
                    .size_full()
                    .absolute(),
                )
                // Needed so `set_cursor_style` above refreshes as the pointer
                // tracks across edge zones.
                .on_mouse_move(|_, window, _| window.refresh())
                .on_mouse_down(MouseButton::Left, |e, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    if let Some(edge) = resize_edge(e.position, EDGE, size) {
                        window.start_window_resize(edge);
                    }
                })
                .child(titlebar(title))
            })
            .child(div().flex_1().min_h_0().child(child))
    }
}

fn titlebar(title: SharedString) -> impl IntoElement {
    div()
        .h(TITLEBAR_HEIGHT)
        .w_full()
        .flex()
        .items_center()
        .px_3()
        .bg(rgb(0x2a2a2a))
        .text_color(rgb(0xaaaaaa))
        .child(title)
        .on_mouse_down(MouseButton::Left, |e, window, _| {
            // The top resize band overlaps the title bar. When the click
            // falls in it, bail out so the outer handler starts a resize
            // rather than a move.
            let size = window.window_bounds().get_bounds().size;
            if resize_edge(e.position, EDGE, size).is_some() {
                return;
            }
            window.start_window_move();
        })
}

fn resize_edge(pos: Point<Pixels>, edge: Pixels, size: Size<Pixels>) -> Option<ResizeEdge> {
    let e = if pos.y < edge && pos.x < edge {
        ResizeEdge::TopLeft
    } else if pos.y < edge && pos.x > size.width - edge {
        ResizeEdge::TopRight
    } else if pos.y < edge {
        ResizeEdge::Top
    } else if pos.y > size.height - edge && pos.x < edge {
        ResizeEdge::BottomLeft
    } else if pos.y > size.height - edge && pos.x > size.width - edge {
        ResizeEdge::BottomRight
    } else if pos.y > size.height - edge {
        ResizeEdge::Bottom
    } else if pos.x < edge {
        ResizeEdge::Left
    } else if pos.x > size.width - edge {
        ResizeEdge::Right
    } else {
        return None;
    };
    Some(e)
}

fn cursor_for(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}
