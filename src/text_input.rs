// Ported and trimmed from Zed's `crates/gpui/examples/input.rs` at rev ec9be5c3.
// When bumping the pinned gpui rev (see Cargo.toml), diff against that file
// first — the IME and Element APIs drift frequently on HEAD.

use std::ops::Range;

use gpui::FontFallbacks;
use gpui::{
    actions, div, fill, hsla, point, prelude::*, px, relative, rgb, rgba, size, white, App, Bounds,
    ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, ShapedLine, SharedString, Style, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window,
};

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Submit,
    ]
);

#[derive(Debug, Clone)]
pub enum TextInputEvent {
    Changed(SharedString),
    Submitted,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub fn new(cx: &mut Context<Self>, placeholder: impl Into<SharedString>) -> Self {
        Self::with_content(cx, placeholder, SharedString::default())
    }

    pub fn with_content(
        cx: &mut Context<Self>,
        placeholder: impl Into<SharedString>,
        content: impl Into<SharedString>,
    ) -> Self {
        let content: SharedString = content.into();
        let end = content.len();
        Self {
            focus_handle: cx.focus_handle(),
            content,
            placeholder: placeholder.into(),
            selected_range: end..end,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
        }
    }

    #[must_use]
    pub fn content(&self) -> &SharedString {
        &self.content
    }

    #[must_use]
    pub fn selected_range(&self) -> Range<usize> {
        self.selected_range.clone()
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(prev_char_boundary(&self.content, self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(
                next_char_boundary(&self.content, self.selected_range.end),
                cx,
            );
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(prev_char_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_char_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        let (new_content, new_selection) =
            apply_backspace(&self.content, self.selected_range.clone());
        if new_content.as_str() == self.content.as_ref() {
            window.play_system_bell();
            return;
        }
        self.apply_edit(new_content, new_selection, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let (new_content, new_selection) = apply_delete(&self.content, self.selected_range.clone());
        if new_content.as_str() == self.content.as_ref() {
            window.play_system_bell();
            return;
        }
        self.apply_edit(new_content, new_selection, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // Single-line input: flatten newlines.
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Submitted);
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn apply_edit(
        &mut self,
        new_content: String,
        new_selection: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = new_selection;
        self.marked_range = None;
        self.set_content_and_emit(new_content, cx);
        cx.notify();
    }

    fn set_content_and_emit(&mut self, new_content: String, cx: &mut Context<Self>) {
        let new_content: SharedString = new_content.into();
        if new_content != self.content {
            self.content = new_content.clone();
            cx.emit(TextInputEvent::Changed(new_content));
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let (new_content, new_selection) = apply_replace(&self.content, range, new_text);
        self.apply_edit(new_content, new_selection, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let new_content = format!(
            "{}{}{}",
            &self.content[..range.start],
            new_text,
            &self.content[range.end..]
        );
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16.as_ref().map_or_else(
            || {
                let end = range.start + new_text.len();
                end..end
            },
            |r| {
                let nr = self.range_from_utf16(r);
                nr.start + range.start..nr.end + range.end
            },
        );
        self.set_content_and_emit(new_content, cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), hsla(0., 0., 0., 0.2))
        } else {
            (content, style.color)
        };

        // Attach platform emoji fonts as fallbacks so codepoints the primary
        // font can't render (e.g. 🦀) still paint, in case a parent element
        // hasn't already set them.
        let mut font = style.font();
        font.fallbacks = Some(emoji_font_fallbacks());
        let run = TextRun {
            len: display_text.len(),
            font,
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    gpui::blue(),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba(0x3311ff30),
                )),
                None,
            )
        };

        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line = prepaint.line.take().unwrap();
        line.paint(
            bounds.origin,
            window.line_height(),
            TextAlign::Left,
            None,
            window,
            cx,
        )
        .unwrap();

        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .bg(rgb(0xeeeeee))
            .line_height(px(28.))
            .text_size(px(16.))
            .child(
                div()
                    .h(px(28. + 4. * 2.))
                    .w_full()
                    .p(px(4.))
                    .bg(white())
                    .child(TextElement { input: cx.entity() }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Platform emoji fonts, ordered so the first installed one wins. GPUI
/// silently skips any family name that isn't present on the system.
#[must_use]
pub fn emoji_font_fallbacks() -> FontFallbacks {
    FontFallbacks::from_fonts(vec![
        "Apple Color Emoji".into(),
        "Noto Color Emoji".into(),
        "Segoe UI Emoji".into(),
    ])
}

/// Register the default keybindings on the `TextInput` context. Call once at
/// startup (see `main.rs`).
pub fn bind_keys(cx: &mut App) {
    let cmd = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("enter", Submit, Some("TextInput")),
        KeyBinding::new(&format!("{cmd}-a"), SelectAll, Some("TextInput")),
        KeyBinding::new(&format!("{cmd}-c"), Copy, Some("TextInput")),
        KeyBinding::new(&format!("{cmd}-v"), Paste, Some("TextInput")),
        KeyBinding::new(&format!("{cmd}-x"), Cut, Some("TextInput")),
    ]);
}

// --- Pure edit core -------------------------------------------------------
// The functions below operate purely on `(content, selection)` pairs, which
// lets us unit-test the edit semantics with no GPUI runtime. View methods
// above delegate to these; view-only concerns (system bell, IME marking,
// clipboard) stay in the methods.

fn prev_char_boundary(s: &str, offset: usize) -> usize {
    let mut i = offset.saturating_sub(1);
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, offset: usize) -> usize {
    let mut i = (offset + 1).min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn apply_replace(content: &str, range: Range<usize>, new_text: &str) -> (String, Range<usize>) {
    let new_content = format!(
        "{}{}{}",
        &content[..range.start],
        new_text,
        &content[range.end..]
    );
    let end = range.start + new_text.len();
    (new_content, end..end)
}

fn apply_backspace(content: &str, selection: Range<usize>) -> (String, Range<usize>) {
    if selection.is_empty() {
        let prev = prev_char_boundary(content, selection.start);
        if prev == selection.start {
            return (content.to_string(), selection);
        }
        apply_replace(content, prev..selection.end, "")
    } else {
        apply_replace(content, selection, "")
    }
}

fn apply_delete(content: &str, selection: Range<usize>) -> (String, Range<usize>) {
    if selection.is_empty() {
        let next = next_char_boundary(content, selection.end);
        if next == selection.end {
            return (content.to_string(), selection);
        }
        apply_replace(content, selection.start..next, "")
    } else {
        apply_replace(content, selection, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_boundaries_handle_multibyte() {
        // "a" (1 byte) + 🦀 (4 bytes) + "b" (1 byte) = 6 bytes
        let s = "a🦀b";
        assert_eq!(prev_char_boundary(s, 6), 5);
        assert_eq!(prev_char_boundary(s, 5), 1);
        assert_eq!(prev_char_boundary(s, 1), 0);
        assert_eq!(prev_char_boundary(s, 0), 0);

        assert_eq!(next_char_boundary(s, 0), 1);
        assert_eq!(next_char_boundary(s, 1), 5);
        assert_eq!(next_char_boundary(s, 5), 6);
        assert_eq!(next_char_boundary(s, 6), 6);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let (out, sel) = apply_backspace("hello", 0..0);
        assert_eq!(out, "hello");
        assert_eq!(sel, 0..0);
    }

    #[test]
    fn backspace_removes_prev_char_when_selection_empty() {
        let (out, sel) = apply_backspace("hello", 3..3);
        assert_eq!(out, "helo");
        assert_eq!(sel, 2..2);
    }

    #[test]
    fn backspace_deletes_selection() {
        let (out, sel) = apply_backspace("hello world", 6..11);
        assert_eq!(out, "hello ");
        assert_eq!(sel, 6..6);
    }

    #[test]
    fn backspace_respects_utf8_boundary() {
        let (out, sel) = apply_backspace("a🦀b", 5..5);
        assert_eq!(out, "ab");
        assert_eq!(sel, 1..1);
    }

    #[test]
    fn delete_at_end_is_noop() {
        let (out, sel) = apply_delete("hello", 5..5);
        assert_eq!(out, "hello");
        assert_eq!(sel, 5..5);
    }

    #[test]
    fn delete_removes_next_char_when_selection_empty() {
        let (out, sel) = apply_delete("hello", 2..2);
        assert_eq!(out, "helo");
        assert_eq!(sel, 2..2);
    }

    #[test]
    fn delete_respects_utf8_boundary() {
        let (out, sel) = apply_delete("a🦀b", 1..1);
        assert_eq!(out, "ab");
        assert_eq!(sel, 1..1);
    }

    #[test]
    fn replace_inserts_at_cursor() {
        let (out, sel) = apply_replace("helo", 3..3, "l");
        assert_eq!(out, "hello");
        assert_eq!(sel, 4..4);
    }

    #[test]
    fn replace_overwrites_selection() {
        let (out, sel) = apply_replace("hello world", 6..11, "there");
        assert_eq!(out, "hello there");
        assert_eq!(sel, 11..11);
    }

    // Runtime smoke test: `TextInput::new` wires up a focus handle and starts
    // with empty content + zero-length selection. Action dispatch / clipboard
    // integration are better exercised manually against the demo view for now.
    #[gpui::test]
    fn new_starts_empty(cx: &mut gpui::TestAppContext) {
        let input = cx.new(|cx| TextInput::new(cx, "placeholder"));
        input.read_with(cx, |input, _| {
            assert_eq!(input.content().as_ref(), "");
            assert_eq!(input.selected_range(), 0..0);
        });
    }
}
