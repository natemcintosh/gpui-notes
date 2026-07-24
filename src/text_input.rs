// Ported and trimmed from Zed's `crates/gpui/examples/input.rs` at rev ec9be5c3.
// When bumping the pinned gpui rev (see Cargo.toml), diff against that file
// first — the IME and Element APIs drift frequently on HEAD.

use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui::FontFallbacks;
use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, AvailableSpace, Bounds,
    ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
    WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        Paste,
        Cut,
        Copy,
        Submit,
        Cancel,
    ]
);

#[derive(Debug, Clone)]
pub enum TextInputEvent {
    Changed(SharedString),
    Submitted,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Selection {
    anchor: usize,
    head: usize,
}

impl Selection {
    fn collapsed(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    fn is_reversed(self) -> bool {
        self.head < self.anchor
    }

    fn collapse(&mut self, offset: usize) {
        self.anchor = offset;
        self.head = offset;
    }

    fn extend_to(&mut self, offset: usize) {
        self.head = offset;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LayoutMode {
    SingleLine,
    SoftWrapped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaretAffinity {
    /// A caret at a soft-wrap boundary is painted at the end of the row above.
    Upstream,
    /// A caret at a soft-wrap boundary is painted at the start of the row below.
    Downstream,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VerticalDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum VerticalMovementOutcome {
    Moved,
    Adjacent {
        direction: VerticalDirection,
        preferred_screen_x: Pixels,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlacementRow {
    First,
    Last,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PendingVerticalPlacement {
    row: PlacementRow,
    preferred_screen_x: Pixels,
}

impl PendingVerticalPlacement {
    #[must_use]
    pub(crate) fn for_navigation(direction: VerticalDirection, preferred_screen_x: Pixels) -> Self {
        let row = match direction {
            VerticalDirection::Up => PlacementRow::Last,
            VerticalDirection::Down => PlacementRow::First,
        };
        Self {
            row,
            preferred_screen_x,
        }
    }
}

struct CachedLayout {
    line: WrappedLine,
    line_height: Pixels,
}

impl CachedLayout {
    fn row_count(&self) -> usize {
        self.line.wrap_boundaries().len() + 1
    }

    fn boundary_index(&self, boundary: usize) -> usize {
        let boundary = self.line.wrap_boundaries()[boundary];
        self.line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index
    }

    fn row_start(&self, row: usize) -> usize {
        if row == 0 {
            0
        } else {
            self.boundary_index(row - 1)
        }
    }

    fn row_end(&self, row: usize) -> usize {
        if row + 1 == self.row_count() {
            self.line.len()
        } else {
            self.boundary_index(row)
        }
    }

    fn row_for_caret(&self, index: usize, affinity: CaretAffinity) -> usize {
        self.line
            .wrap_boundaries()
            .iter()
            .enumerate()
            .find_map(|(row, _)| {
                let boundary = self.boundary_index(row);
                if index < boundary || (index == boundary && affinity == CaretAffinity::Upstream) {
                    Some(row)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.row_count() - 1)
    }

    fn row_for_y(&self, y: Pixels) -> usize {
        let mut row = 0;
        while row + 1 < self.row_count() && y >= self.line_height * (row + 1) {
            row += 1;
        }
        row
    }

    fn position_for_caret(&self, index: usize, affinity: CaretAffinity) -> Point<Pixels> {
        let row = self.row_for_caret(index, affinity);
        let row_start = self.row_start(row);
        point(
            self.line.unwrapped_layout.x_for_index(index)
                - self.line.unwrapped_layout.x_for_index(row_start),
            self.line_height * row,
        )
    }

    fn caret_for_row_x(&self, row: usize, x: Pixels) -> (usize, CaretAffinity) {
        let row = row.min(self.row_count() - 1);
        let position = point(x, self.line_height * row + self.line_height / 2.);
        let index = self
            .line
            .closest_index_for_position(position, self.line_height)
            .unwrap_or_else(|index| index);
        let affinity = if row > 0 && index == self.row_start(row) {
            CaretAffinity::Downstream
        } else {
            CaretAffinity::Upstream
        };
        (index, affinity)
    }

    fn selection_quads(&self, selection: Range<usize>, bounds: Bounds<Pixels>) -> Vec<PaintQuad> {
        let mut quads = Vec::new();
        for row in 0..self.row_count() {
            let row_start = self.row_start(row);
            let row_end = self.row_end(row);
            let start = selection.start.max(row_start);
            let end = selection.end.min(row_end);
            if start >= end {
                continue;
            }
            let start_x = self.line.unwrapped_layout.x_for_index(row_start);
            let selection_left = self.line.unwrapped_layout.x_for_index(start) - start_x;
            let selection_right = self.line.unwrapped_layout.x_for_index(end) - start_x;
            quads.push(fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + selection_left,
                        bounds.top() + self.line_height * row,
                    ),
                    point(
                        bounds.left() + selection_right,
                        bounds.top() + self.line_height * (row + 1),
                    ),
                ),
                theme::input_selection(),
            ));
        }
        quads
    }
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selection: Selection,
    caret_affinity: CaretAffinity,
    preferred_screen_x: Option<Pixels>,
    pending_vertical_placement: Option<PendingVerticalPlacement>,
    layout_mode: LayoutMode,
    marked_range: Option<Range<usize>>,
    last_layout: Option<CachedLayout>,
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
        Self::with_layout(cx, placeholder, content, LayoutMode::SingleLine)
    }

    pub(crate) fn with_wrapped_content(
        cx: &mut Context<Self>,
        placeholder: impl Into<SharedString>,
        content: impl Into<SharedString>,
    ) -> Self {
        Self::with_layout(cx, placeholder, content, LayoutMode::SoftWrapped)
    }

    fn with_layout(
        cx: &mut Context<Self>,
        placeholder: impl Into<SharedString>,
        content: impl Into<SharedString>,
        layout_mode: LayoutMode,
    ) -> Self {
        let content: SharedString = content.into();
        let end = content.len();
        Self {
            focus_handle: cx.focus_handle(),
            content,
            placeholder: placeholder.into(),
            selection: Selection::collapsed(end),
            caret_affinity: CaretAffinity::Upstream,
            preferred_screen_x: None,
            pending_vertical_placement: None,
            layout_mode,
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
        self.selection.range()
    }

    pub(crate) fn set_pending_vertical_placement(&mut self, placement: PendingVerticalPlacement) {
        self.preferred_screen_x = Some(placement.preferred_screen_x);
        self.pending_vertical_placement = Some(placement);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(character_left_target(&self.content, self.selection), cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(character_right_target(&self.content, self.selection), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(
            previous_grapheme_boundary(&self.content, self.cursor_offset()),
            cx,
        );
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(
            next_grapheme_boundary(&self.content, self.cursor_offset()),
            cx,
        );
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(previous_word_start(&self.content, self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(next_word_end(&self.content, self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_word_start(&self.content, self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_word_end(&self.content, self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = Selection {
            anchor: 0,
            head: self.content.len(),
        };
        self.caret_affinity = CaretAffinity::Upstream;
        self.preferred_screen_x = None;
        self.pending_vertical_placement = None;
        cx.notify();
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        let (new_content, new_selection) = apply_backspace(&self.content, self.selection.range());
        if new_content.as_str() == self.content.as_ref() {
            window.play_system_bell();
            return;
        }
        self.apply_edit(new_content, new_selection, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let (new_content, new_selection) = apply_delete(&self.content, self.selection.range());
        if new_content.as_str() == self.content.as_ref() {
            window.play_system_bell();
            return;
        }
        self.apply_edit(new_content, new_selection, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // The buffer is one logical line even when it is visually wrapped.
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected_range = self.selection.range();
        if !selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[selected_range].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        let selected_range = self.selection.range();
        if !selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[selected_range].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Submitted);
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Cancelled);
    }

    /// Replaces the entire buffer in a headless test. Goes through the same
    /// `apply_edit` path as real user edits, so subscribers receive a normal
    /// `Changed` event.
    #[cfg(test)]
    pub fn test_replace_all(&mut self, new_content: impl Into<String>, cx: &mut Context<Self>) {
        let content: String = new_content.into();
        let end = content.len();
        self.apply_edit(content, end..end, cx);
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        let (offset, affinity) = self.caret_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_to_with_affinity(offset, affinity, cx);
        } else {
            self.move_to_with_affinity(offset, affinity, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let (offset, affinity) = self.caret_for_mouse_position(event.position);
            self.select_to_with_affinity(offset, affinity, cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.move_to_with_affinity(offset, CaretAffinity::Upstream, cx);
    }

    fn move_to_with_affinity(
        &mut self,
        offset: usize,
        affinity: CaretAffinity,
        cx: &mut Context<Self>,
    ) {
        self.selection.collapse(offset);
        self.caret_affinity = affinity;
        self.preferred_screen_x = None;
        self.pending_vertical_placement = None;
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        self.selection.head
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.select_to_with_affinity(offset, CaretAffinity::Upstream, cx);
    }

    fn select_to_with_affinity(
        &mut self,
        offset: usize,
        affinity: CaretAffinity,
        cx: &mut Context<Self>,
    ) {
        self.selection.extend_to(offset);
        self.caret_affinity = affinity;
        self.preferred_screen_x = None;
        self.pending_vertical_placement = None;
        cx.notify();
    }

    fn caret_for_mouse_position(&self, position: Point<Pixels>) -> (usize, CaretAffinity) {
        if self.content.is_empty() {
            return (0, CaretAffinity::Upstream);
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return (0, CaretAffinity::Upstream);
        };
        if position.y < bounds.top() {
            return (0, CaretAffinity::Upstream);
        }
        if position.y > bounds.bottom() {
            return (self.content.len(), CaretAffinity::Upstream);
        }
        let row = line.row_for_y(position.y - bounds.top());
        line.caret_for_row_x(row, position.x - bounds.left())
    }

    pub(crate) fn move_vertical(
        &mut self,
        direction: VerticalDirection,
        cx: &mut Context<Self>,
    ) -> VerticalMovementOutcome {
        let Some(bounds) = self.last_bounds else {
            return VerticalMovementOutcome::Moved;
        };
        let Some(layout) = self.last_layout.as_ref() else {
            return VerticalMovementOutcome::Moved;
        };

        // Plain vertical movement always starts at the active head. Unlike
        // horizontal arrows, the selected edge is not chosen by direction.
        self.selection.collapse(self.selection.head);
        let current_position = layout.position_for_caret(self.selection.head, self.caret_affinity);
        let preferred_screen_x = self
            .preferred_screen_x
            .unwrap_or(bounds.left() + current_position.x);
        self.preferred_screen_x = Some(preferred_screen_x);

        let current_row = layout.row_for_caret(self.selection.head, self.caret_affinity);
        let target_row = match direction {
            VerticalDirection::Up => current_row.checked_sub(1),
            VerticalDirection::Down => {
                (current_row + 1 < layout.row_count()).then_some(current_row + 1)
            }
        };

        let Some(target_row) = target_row else {
            cx.notify();
            return VerticalMovementOutcome::Adjacent {
                direction,
                preferred_screen_x,
            };
        };

        let (offset, affinity) =
            layout.caret_for_row_x(target_row, preferred_screen_x - bounds.left());
        self.selection.collapse(offset);
        self.caret_affinity = affinity;
        cx.notify();
        VerticalMovementOutcome::Moved
    }

    fn apply_pending_vertical_placement(&mut self, cx: &mut Context<Self>) {
        let (Some(placement), Some(bounds), Some(layout)) = (
            self.pending_vertical_placement.take(),
            self.last_bounds,
            self.last_layout.as_ref(),
        ) else {
            return;
        };
        let row = match placement.row {
            PlacementRow::First => 0,
            PlacementRow::Last => layout.row_count() - 1,
        };
        let (offset, affinity) =
            layout.caret_for_row_x(row, placement.preferred_screen_x - bounds.left());
        self.selection.collapse(offset);
        self.caret_affinity = affinity;
        self.preferred_screen_x = Some(placement.preferred_screen_x);
        cx.notify();
    }

    fn apply_edit(
        &mut self,
        new_content: String,
        new_selection: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(new_selection.is_empty());
        self.selection = Selection::collapsed(new_selection.end);
        self.caret_affinity = CaretAffinity::Upstream;
        self.preferred_screen_x = None;
        self.pending_vertical_placement = None;
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
        offset_from_utf16_in(&self.content, offset)
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
        let selected_range = self.selection.range();
        Some(UTF16Selection {
            range: self.range_to_utf16(&selected_range),
            reversed: self.selection.is_reversed(),
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
            .unwrap_or_else(|| self.selection.range());

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
            .unwrap_or_else(|| self.selection.range());

        let new_content = format!(
            "{}{}{}",
            &self.content[..range.start],
            new_text,
            &self.content[range.end..]
        );
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        let cursor = new_selected_range_utf16.as_ref().map_or_else(
            || range.start + new_text.len(),
            |relative_range| range.start + offset_from_utf16_in(new_text, relative_range.end),
        );
        self.selection = Selection::collapsed(cursor);
        self.caret_affinity = CaretAffinity::Upstream;
        self.preferred_screen_x = None;
        self.pending_vertical_placement = None;
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
        let (start_affinity, end_affinity) = if range.is_empty() {
            (self.caret_affinity, self.caret_affinity)
        } else {
            (CaretAffinity::Downstream, CaretAffinity::Upstream)
        };
        let start = last_layout.position_for_caret(range.start, start_affinity);
        let end = last_layout.position_for_caret(range.end, end_affinity);
        let top = bounds.top() + start.y;
        let bottom = bounds.top() + end.y + last_layout.line_height;
        if start.y == end.y {
            Some(Bounds::from_corners(
                point(bounds.left() + start.x, top),
                point(bounds.left() + end.x.max(start.x + px(2.)), bottom),
            ))
        } else {
            Some(Bounds::from_corners(
                point(bounds.left(), top),
                point(bounds.right(), bottom),
            ))
        }
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let last_layout = self.last_layout.as_ref()?;
        let row = last_layout.row_for_y(point.y - bounds.top());
        let (utf8_index, _) = last_layout.caret_for_row_x(row, point.x - bounds.left());
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

type MeasuredLine = Rc<RefCell<Option<(WrappedLine, Pixels)>>>;

struct PrepaintState {
    layout: Option<CachedLayout>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = MeasuredLine;
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
        let input = self.input.read(cx);
        let content = input.content.clone();
        let layout_mode = input.layout_mode;
        let marked_range = input.marked_range.clone();
        let style = window.text_style();
        let (display_text, text_color) = if content.is_empty() {
            (
                input.placeholder.clone(),
                theme::input_placeholder_fg().into(),
            )
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
        let runs = if let Some(marked_range) = marked_range {
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
            .filter(|run| run.len > 0)
            .collect::<Vec<_>>()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let measured: MeasuredLine = Rc::default();
        let measured_for_layout = measured.clone();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id = window.request_measured_layout(
            style,
            move |known_dimensions, available_space, window, _cx| {
                let wrap_width = if layout_mode == LayoutMode::SoftWrapped {
                    known_dimensions.width.or(match available_space.width {
                        AvailableSpace::Definite(width) => Some(width),
                        _ => None,
                    })
                } else {
                    None
                };
                let line = window
                    .text_system()
                    .shape_text(display_text.clone(), font_size, &runs, wrap_width, None)
                    .expect("text shaping should succeed")
                    .into_iter()
                    .next()
                    .expect("shape_text always returns a logical line");
                let measured_size = line.size(line_height);
                measured_for_layout
                    .borrow_mut()
                    .replace((line, line_height));
                measured_size
            },
        );
        (layout_id, measured)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        measured: &mut Self::RequestLayoutState,
        _: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let selected_range = input.selection.range();
        let cursor = input.cursor_offset();
        let (line, line_height) = measured
            .borrow_mut()
            .take()
            .expect("measured text layout should be available during prepaint");
        let layout = CachedLayout { line, line_height };
        let cursor_pos = layout.position_for_caret(cursor, input.caret_affinity);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                Vec::new(),
                Some(fill(
                    Bounds::new(bounds.origin + cursor_pos, size(px(2.), line_height)),
                    theme::input_cursor(),
                )),
            )
        } else {
            (layout.selection_quads(selected_range, bounds), None)
        };

        PrepaintState {
            layout: Some(layout),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
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
        for selection in prepaint.selection.drain(..) {
            window.paint_quad(selection);
        }
        let layout = prepaint.layout.take().unwrap();
        layout
            .line
            .paint(
                bounds.origin,
                layout.line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            )
            .unwrap();

        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        self.input.update(cx, |input, cx| {
            input.last_layout = Some(layout);
            input.last_bounds = Some(bounds);
            input.apply_pending_vertical_placement(cx);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .bg(theme::input_outer_bg())
            .text_color(theme::input_fg())
            .line_height(px(28.))
            .text_size(px(16.))
            .child(
                div()
                    .min_h(px(28. + 4. * 2.))
                    .w_full()
                    .p(px(4.))
                    .bg(theme::input_bg())
                    .child(TextElement { input: cx.entity() }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
impl TextInput {
    pub(crate) fn cursor_offset_for_test(&self) -> usize {
        self.cursor_offset()
    }

    pub(crate) fn visual_row_for_test(&self) -> Option<usize> {
        self.last_layout
            .as_ref()
            .map(|layout| layout.row_for_caret(self.cursor_offset(), self.caret_affinity))
    }

    pub(crate) fn visual_row_count_for_test(&self) -> Option<usize> {
        self.last_layout.as_ref().map(CachedLayout::row_count)
    }

    pub(crate) fn visual_row_width_for_test(&self, row: usize) -> Option<Pixels> {
        let layout = self.last_layout.as_ref()?;
        let start = layout.row_start(row);
        let end = layout.row_end(row);
        Some(
            layout.line.unwrapped_layout.x_for_index(end)
                - layout.line.unwrapped_layout.x_for_index(start),
        )
    }

    pub(crate) fn cursor_screen_position_for_test(&self) -> Option<Point<Pixels>> {
        let bounds = self.last_bounds?;
        let layout = self.last_layout.as_ref()?;
        Some(bounds.origin + layout.position_for_caret(self.cursor_offset(), self.caret_affinity))
    }

    pub(crate) fn layout_bounds_for_test(&self) -> Option<Bounds<Pixels>> {
        self.last_bounds
    }

    pub(crate) fn line_height_for_test(&self) -> Option<Pixels> {
        self.last_layout.as_ref().map(|layout| layout.line_height)
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
    let cmd = crate::cmd_key();

    // Register the primary native motions before their aliases so the
    // shortcut bar presents the most familiar spelling first.
    cx.bind_keys([
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
    ]);

    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("alt-left", WordLeft, Some("TextInput")),
        KeyBinding::new("alt-right", WordRight, Some("TextInput")),
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("alt-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("cmd-left", Home, Some("TextInput")),
        KeyBinding::new("cmd-right", End, Some("TextInput")),
        KeyBinding::new("cmd-shift-left", SelectHome, Some("TextInput")),
        KeyBinding::new("cmd-shift-right", SelectEnd, Some("TextInput")),
    ]);

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-left", WordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-right", WordRight, Some("TextInput")),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("TextInput")),
    ]);

    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("ctrl-b", Left, Some("TextInput")),
        KeyBinding::new("ctrl-f", Right, Some("TextInput")),
        KeyBinding::new("ctrl-shift-b", SelectLeft, Some("TextInput")),
        KeyBinding::new("ctrl-shift-f", SelectRight, Some("TextInput")),
        KeyBinding::new("ctrl-a", Home, Some("TextInput")),
        KeyBinding::new("ctrl-e", End, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("cmd-up", Home, Some("TextInput")),
        KeyBinding::new("cmd-down", End, Some("TextInput")),
        KeyBinding::new("cmd-home", Home, Some("TextInput")),
        KeyBinding::new("cmd-end", End, Some("TextInput")),
        KeyBinding::new("ctrl-shift-a", SelectHome, Some("TextInput")),
        KeyBinding::new("ctrl-shift-e", SelectEnd, Some("TextInput")),
        KeyBinding::new("shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("TextInput")),
        KeyBinding::new("cmd-shift-up", SelectHome, Some("TextInput")),
        KeyBinding::new("cmd-shift-down", SelectEnd, Some("TextInput")),
        KeyBinding::new("cmd-shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("cmd-shift-end", SelectEnd, Some("TextInput")),
    ]);

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-home", Home, Some("TextInput")),
        KeyBinding::new("ctrl-end", End, Some("TextInput")),
        KeyBinding::new("ctrl-shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("ctrl-shift-end", SelectEnd, Some("TextInput")),
    ]);

    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("enter", Submit, Some("TextInput")),
        KeyBinding::new("escape", Cancel, Some("TextInput")),
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

fn previous_grapheme_boundary(content: &str, offset: usize) -> usize {
    content
        .grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}

fn next_grapheme_boundary(content: &str, offset: usize) -> usize {
    content
        .grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(content.len())
}

fn character_left_target(content: &str, selection: Selection) -> usize {
    if selection.is_empty() {
        previous_grapheme_boundary(content, selection.head)
    } else {
        selection.range().start
    }
}

fn character_right_target(content: &str, selection: Selection) -> usize {
    if selection.is_empty() {
        next_grapheme_boundary(content, selection.head)
    } else {
        selection.range().end
    }
}

fn previous_word_start(content: &str, offset: usize) -> usize {
    content
        .unicode_word_indices()
        .take_while(|(start, _)| *start < offset)
        .map(|(start, _)| start)
        .last()
        .unwrap_or(0)
}

fn next_word_end(content: &str, offset: usize) -> usize {
    content
        .unicode_word_indices()
        .map(|(start, word)| start + word.len())
        .find(|end| *end > offset)
        .unwrap_or(content.len())
}

fn offset_from_utf16_in(content: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for ch in content.chars() {
        if utf16 >= offset {
            break;
        }
        utf16 += ch.len_utf16();
        utf8 += ch.len_utf8();
    }
    utf8
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
        let prev = previous_grapheme_boundary(content, selection.start);
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
        let next = next_grapheme_boundary(content, selection.end);
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
    use gpui::{size, Entity, Focusable, Modifiers, TestAppContext, VisualTestContext};
    use rstest::rstest;

    #[rstest]
    #[case::ascii("abc", 1, 0, 2)]
    #[case::multibyte("a🦀b", 1, 0, 5)]
    #[case::combining_mark("ae\u{301}b", 1, 0, 4)]
    #[case::at_end("a🦀b", 6, 5, 6)]
    #[case::at_start("a🦀b", 0, 0, 1)]
    fn grapheme_boundaries(
        #[case] content: &str,
        #[case] offset: usize,
        #[case] expected_previous: usize,
        #[case] expected_next: usize,
    ) {
        assert_eq!(
            previous_grapheme_boundary(content, offset),
            expected_previous
        );
        assert_eq!(next_grapheme_boundary(content, offset), expected_next);
    }

    #[test]
    fn grapheme_boundaries_keep_joined_emoji_together() {
        let family = "👩‍👩‍👧‍👦";
        let content = format!("a{family}b");
        let family_start = 1;
        let family_end = family_start + family.len();

        assert_eq!(next_grapheme_boundary(&content, family_start), family_end);
        assert_eq!(
            previous_grapheme_boundary(&content, family_end),
            family_start
        );
    }

    #[rstest]
    #[case::empty("", 0, 0, 0)]
    #[case::only_separators("   ", 1, 0, 3)]
    #[case::leading_whitespace("  hello", 1, 0, 7)]
    #[case::buffer_start("hello world", 0, 0, 5)]
    #[case::inside_word("hello world", 2, 0, 5)]
    #[case::at_word_end("hello world", 5, 0, 11)]
    #[case::between_words("hello,  world", 7, 0, 13)]
    #[case::at_next_word_start("hello,  world", 8, 0, 13)]
    #[case::underscore_word("one_two three", 4, 0, 7)]
    #[case::punctuation("foo—bar", 3, 0, 9)]
    #[case::inside_unicode_word("naïve 東京", 3, 0, 6)]
    #[case::at_unicode_word_start("naïve 東京", 7, 0, 10)]
    #[case::buffer_end("naïve 東京", 13, 10, 13)]
    fn word_boundaries(
        #[case] content: &str,
        #[case] offset: usize,
        #[case] expected_left: usize,
        #[case] expected_right: usize,
    ) {
        assert_eq!(previous_word_start(content, offset), expected_left);
        assert_eq!(next_word_end(content, offset), expected_right);
    }

    #[test]
    fn selection_extends_shrinks_and_crosses_its_anchor() {
        let mut selection = Selection::collapsed(5);

        selection.extend_to(9);
        assert_eq!(selection.range(), 5..9);
        assert!(!selection.is_reversed());

        selection.extend_to(7);
        assert_eq!(selection.range(), 5..7);

        selection.extend_to(3);
        assert_eq!(selection.range(), 3..5);
        assert!(selection.is_reversed());

        selection.extend_to(8);
        assert_eq!(selection.range(), 5..8);
        assert!(!selection.is_reversed());
    }

    #[test]
    fn character_motion_collapses_to_directional_selection_edge() {
        let content = "a🦀b";
        let forward = Selection { anchor: 1, head: 5 };
        let reversed = Selection { anchor: 5, head: 1 };

        assert_eq!(character_left_target(content, forward), 1);
        assert_eq!(character_right_target(content, forward), 5);
        assert_eq!(character_left_target(content, reversed), 1);
        assert_eq!(character_right_target(content, reversed), 5);
    }

    #[rstest]
    #[case::at_start_is_noop("hello", 0..0, "hello", 0..0)]
    #[case::removes_prev_char_when_selection_empty("hello", 3..3, "helo", 2..2)]
    #[case::deletes_selection("hello world", 6..11, "hello ", 6..6)]
    #[case::respects_utf8_boundary("a🦀b", 5..5, "ab", 1..1)]
    #[case::removes_combining_grapheme("ae\u{301}b", 4..4, "ab", 1..1)]
    fn backspace_cases(
        #[case] content: &str,
        #[case] selection: Range<usize>,
        #[case] expected_content: &str,
        #[case] expected_selection: Range<usize>,
    ) {
        let (out, sel) = apply_backspace(content, selection);
        assert_eq!(out, expected_content);
        assert_eq!(sel, expected_selection);
    }

    #[rstest]
    #[case::at_end_is_noop("hello", 5..5, "hello", 5..5)]
    #[case::removes_next_char_when_selection_empty("hello", 2..2, "helo", 2..2)]
    #[case::respects_utf8_boundary("a🦀b", 1..1, "ab", 1..1)]
    #[case::removes_combining_grapheme("ae\u{301}b", 1..1, "ab", 1..1)]
    fn delete_cases(
        #[case] content: &str,
        #[case] selection: Range<usize>,
        #[case] expected_content: &str,
        #[case] expected_selection: Range<usize>,
    ) {
        let (out, sel) = apply_delete(content, selection);
        assert_eq!(out, expected_content);
        assert_eq!(sel, expected_selection);
    }

    #[rstest]
    #[case::inserts_at_cursor("helo", 3..3, "l", "hello", 4..4)]
    #[case::overwrites_selection("hello world", 6..11, "there", "hello there", 11..11)]
    fn replace_cases(
        #[case] content: &str,
        #[case] range: Range<usize>,
        #[case] new_text: &str,
        #[case] expected_content: &str,
        #[case] expected_selection: Range<usize>,
    ) {
        let (out, sel) = apply_replace(content, range, new_text);
        assert_eq!(out, expected_content);
        assert_eq!(sel, expected_selection);
    }

    fn mount_input<'a>(
        cx: &'a mut TestAppContext,
        content: &str,
    ) -> (Entity<TextInput>, &'a mut VisualTestContext) {
        let content = content.to_string();
        let (input, vcx) = cx.add_window_view(move |_window, cx| {
            bind_keys(cx);
            TextInput::with_content(cx, "", content)
        });
        vcx.update(|window, cx| {
            window.activate_window();
            window.focus(&input.focus_handle(cx), cx);
        });
        vcx.run_until_parked();
        (input, vcx)
    }

    fn mount_wrapped_input<'a>(
        cx: &'a mut TestAppContext,
        content: &str,
    ) -> (Entity<TextInput>, &'a mut VisualTestContext) {
        let content = content.to_string();
        let (input, vcx) = cx.add_window_view(move |_window, cx| {
            bind_keys(cx);
            TextInput::with_wrapped_content(cx, "", content)
        });
        vcx.simulate_resize(size(px(220.), px(500.)));
        vcx.update(|window, cx| {
            window.activate_window();
            window.focus(&input.focus_handle(cx), cx);
            window.draw(cx).clear();
        });
        vcx.run_until_parked();
        (input, vcx)
    }

    #[gpui::test]
    fn new_starts_empty(cx: &mut TestAppContext) {
        let input = cx.new(|cx| TextInput::new(cx, "placeholder"));
        input.read_with(cx, |input, _| {
            assert_eq!(input.content().as_ref(), "");
            assert_eq!(input.selected_range(), 0..0);
        });
    }

    #[gpui::test]
    fn key_motion_preserves_graphemes_and_anchor_semantics(cx: &mut TestAppContext) {
        let (input, cx) = mount_input(cx, "ae\u{301}b");

        cx.simulate_keystrokes("home right right");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(
                input.selected_range(),
                4..4,
                "right crosses the combining sequence as one grapheme",
            );
        });

        cx.simulate_keystrokes("shift-left");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 1..4);
            assert!(input.selection.is_reversed());
        });

        cx.simulate_keystrokes("shift-right shift-right");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(
                input.selected_range(),
                4..5,
                "selection shrinks to its anchor, then grows past it",
            );
            assert!(!input.selection.is_reversed());
        });
    }

    #[gpui::test]
    fn unmodified_character_motion_collapses_selection_directionally(cx: &mut TestAppContext) {
        let (input, cx) = mount_input(cx, "abcd");

        cx.simulate_keystrokes("home shift-right shift-right left");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 0..0);
        });

        cx.simulate_keystrokes("shift-right shift-right right");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 2..2);
        });
    }

    #[gpui::test]
    fn endpoint_selection_can_reverse_across_anchor(cx: &mut TestAppContext) {
        let (input, cx) = mount_input(cx, "abcd");

        cx.simulate_keystrokes("home right right shift-home");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 0..2);
            assert!(input.selection.is_reversed());
        });

        cx.simulate_keystrokes("shift-end");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 2..4);
            assert!(!input.selection.is_reversed());
        });
    }

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn macos_native_word_and_alias_bindings(cx: &mut TestAppContext) {
        let (input, cx) = mount_input(cx, "alpha beta");

        cx.simulate_keystrokes("alt-left alt-shift-left");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 0..6);
            assert!(input.selection.is_reversed());
        });

        cx.simulate_keystrokes("cmd-shift-right");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 6..10);
        });

        cx.simulate_keystrokes("ctrl-a ctrl-shift-f");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 0..1);
        });
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[gpui::test]
    fn unix_native_word_and_endpoint_bindings(cx: &mut TestAppContext) {
        let (input, cx) = mount_input(cx, "alpha beta");

        cx.simulate_keystrokes("ctrl-left ctrl-shift-left");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 0..6);
            assert!(input.selection.is_reversed());
        });

        cx.simulate_keystrokes("ctrl-shift-end");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.selected_range(), 6..10);
        });
    }

    #[gpui::test]
    fn edits_and_ime_replacements_collapse_selection(cx: &mut TestAppContext) {
        let (input, cx) = mount_input(cx, "abc");

        cx.simulate_keystrokes("home shift-right shift-right");
        cx.simulate_input("X");
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.content().as_ref(), "Xc");
            assert_eq!(input.selected_range(), 1..1);
        });

        cx.simulate_keystrokes("home shift-right");
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "🦀", Some(2..2), window, cx);
            });
        });
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.content().as_ref(), "🦀c");
            assert_eq!(input.selected_range(), 4..4);
            assert_eq!(input.marked_range, Some(0..4));
        });

        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "x", window, cx);
            });
        });
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            assert_eq!(input.content().as_ref(), "xc");
            assert_eq!(input.selected_range(), 1..1);
            assert_eq!(input.marked_range, None);
        });
    }

    #[gpui::test]
    fn wrapped_layout_grows_and_clicks_later_unicode_rows(cx: &mut TestAppContext) {
        let content = "alpha 🦀 e\u{301} beta gamma delta epsilon zeta eta theta iota kappa lambda";
        let (input, cx) = mount_wrapped_input(cx, content);

        let click = input.read_with(cx, |input, _| {
            let rows = input
                .visual_row_count_for_test()
                .expect("wrapped layout painted");
            let bounds = input.layout_bounds_for_test().expect("layout bounds");
            let line_height = input.line_height_for_test().expect("line height");
            assert!(rows > 1, "narrow host should produce visual wrapping");
            assert!(
                bounds.size.height > line_height,
                "wrapped editor height should grow with its visual rows",
            );
            assert_eq!(
                input
                    .last_layout
                    .as_ref()
                    .expect("cached layout")
                    .selection_quads(0..content.len(), bounds)
                    .len(),
                rows,
                "a full selection should paint one highlight per visual row",
            );
            point(
                bounds.left() + px(32.),
                bounds.top() + line_height + line_height / 2.,
            )
        });

        cx.simulate_click(click, Modifiers::default());
        cx.run_until_parked();

        input.read_with(cx, |input, _| {
            let cursor = input.cursor_offset_for_test();
            assert_eq!(
                input.visual_row_for_test(),
                Some(1),
                "click should place the caret on the second visual row",
            );
            assert!(
                cursor == content.len()
                    || content
                        .grapheme_indices(true)
                        .any(|(boundary, _)| boundary == cursor),
                "mouse placement must remain on a Unicode grapheme boundary",
            );
        });
    }

    #[gpui::test]
    fn wrapped_ime_bounds_follow_caret_affinity_at_wrap_boundary(cx: &mut TestAppContext) {
        let (input, cx) = mount_wrapped_input(
            cx,
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda",
        );

        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                let layout = input.last_layout.as_ref().expect("wrapped layout painted");
                assert!(layout.row_count() > 1, "precondition: input wraps");
                let boundary = layout.boundary_index(0);
                let line_height = layout.line_height;
                let input_bounds = input.last_bounds.expect("input bounds");
                input.selection = Selection::collapsed(boundary);
                input.caret_affinity = CaretAffinity::Downstream;
                let boundary_utf16 = input.offset_to_utf16(boundary);

                let ime_bounds = input
                    .bounds_for_range(boundary_utf16..boundary_utf16, input_bounds, window, cx)
                    .expect("IME bounds");
                assert_eq!(
                    ime_bounds.top(),
                    input_bounds.top() + line_height,
                    "downstream affinity puts the IME caret on the next row",
                );
                assert_eq!(ime_bounds.size.height, line_height);
            });
        });
    }

    #[gpui::test]
    fn regular_constructor_remains_single_line_when_narrow(cx: &mut TestAppContext) {
        let (input, cx) = mount_input(
            cx,
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda",
        );
        cx.simulate_resize(size(px(180.), px(300.)));
        cx.run_until_parked();

        input.read_with(cx, |input, _| {
            assert_eq!(input.visual_row_count_for_test(), Some(1));
            assert_eq!(
                input
                    .layout_bounds_for_test()
                    .map(|bounds| bounds.size.height),
                input.line_height_for_test(),
                "page-picker-style inputs must retain one-line height",
            );
        });
    }
}
