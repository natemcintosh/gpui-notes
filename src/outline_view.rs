//! Outline-level container that composes `BlockView` children with indent +
//! bullet glyphs. See issue #6.
//!
//! `BlockView` entities are cached by `BlockId` so focus state and any
//! mounted `TextInput` survive re-renders. Ids missing from the page's
//! outline (deleted blocks) are pruned at render time.

use std::collections::HashMap;

use gpui::{
    actions, anchored, deferred, div, px, relative, AnyElement, App, AppContext, ClipboardItem,
    Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window,
};

use crate::block_render::theme;
use crate::block_view::{BlockView, BlockViewEvent};
use crate::linked_refs::LinkedRefsView;
use crate::outline::{Block, BlockId, Outline};
use crate::page::Page;
use crate::text_input::{PendingVerticalPlacement, VerticalDirection};

actions!(
    outline_view,
    [
        FocusUp,
        FocusDown,
        IndentBlock,
        OutdentBlock,
        MoveBlockUp,
        MoveBlockDown,
        ToggleCollapse,
        DeleteBlock,
        UndoDelete
    ]
);

/// Logseq's geometry: each nesting level steps 30px right, and a block's
/// text starts 23px past its bullet column's left edge (this column plus the
/// `BlockView`'s 1px border and 2px padding).
const INDENT_PER_LEVEL: Pixels = px(30.0);
const BULLET_COLUMN_WIDTH: Pixels = px(20.0);
/// Logseq's vertical rhythm: lines at 1.5× the font size, and single-line
/// rows 7px further apart than that — this gap plus the `BlockView`'s 2px
/// padding and 1px border, top and bottom.
const LINE_HEIGHT: f32 = 1.5;
const ROW_GAP: Pixels = px(1.0);
/// The bullet dot sits on the centre of a block's first line of body text:
/// the `BlockView`'s 1px border and 2px padding, plus half a 24px line.
const BULLET_CENTER_Y: f32 = 15.0;
const BULLET_SIZE: f32 = 6.0;
/// The ring drawn behind a folded block's bullet, Logseq-style.
const FOLDED_RING_SIZE: f32 = 16.0;
/// The fold arrow sits just left of the bullet column, in the indent.
const FOLD_TOGGLE_WIDTH: Pixels = px(16.0);
/// Every row is a hover group so its fold arrow can appear on hover.
const ROW_GROUP: &str = "outline-row";
/// How sharply a thread turns from its parent's column into a bullet.
const THREAD_RADIUS: Pixels = px(8.0);

/// The outline navigation/operation keys act on the focused-but-viewing
/// block (#42/#44). The `!editing` guard keeps them from firing while that
/// block's `TextInput` is focused — the wrapper (and its key context) is an
/// ancestor of the input, so without the guard, keys the input doesn't bind
/// (up/down/tab…) would leak into outline ops mid-edit.
const BLOCK_CONTEXT: &str = "BlockView && !editing";
const EDITING_CONTEXT: &str = "BlockView && editing";

pub fn bind_keys(cx: &mut App) {
    let cmd = crate::cmd_key();
    cx.bind_keys([
        KeyBinding::new("up", FocusUp, Some(BLOCK_CONTEXT)),
        KeyBinding::new("down", FocusDown, Some(BLOCK_CONTEXT)),
        KeyBinding::new("tab", IndentBlock, Some(BLOCK_CONTEXT)),
        KeyBinding::new("shift-tab", OutdentBlock, Some(BLOCK_CONTEXT)),
        KeyBinding::new("alt-up", MoveBlockUp, Some(BLOCK_CONTEXT)),
        KeyBinding::new("alt-down", MoveBlockDown, Some(BLOCK_CONTEXT)),
        KeyBinding::new(&format!("{cmd}-enter"), ToggleCollapse, Some(BLOCK_CONTEXT)),
        KeyBinding::new("backspace", DeleteBlock, Some(BLOCK_CONTEXT)),
        KeyBinding::new("delete", DeleteBlock, Some(BLOCK_CONTEXT)),
        KeyBinding::new(&format!("{cmd}-z"), UndoDelete, Some(BLOCK_CONTEXT)),
        // Mid-edit, Tab indents only from the very start of the block (the
        // handler checks the caret); elsewhere it does nothing.
        KeyBinding::new("tab", IndentBlock, Some(EDITING_CONTEXT)),
        KeyBinding::new("shift-tab", OutdentBlock, Some(EDITING_CONTEXT)),
    ]);
}

/// Which slice of the page this view renders.
///
/// `Subtree` views back the linked-references rows: they show one block and
/// its descendants from some *other* page, are not their own scroll root, and
/// carry no references section of their own. `Embedded` views back the days
/// in journals mode: the whole page, but likewise inside someone else's
/// scroll container and without references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutlineScope {
    Page,
    Embedded,
    Subtree(BlockId),
}

/// The menu a right-click on a bullet opens, at the click position.
struct BulletMenu {
    block: BlockId,
    position: Point<Pixels>,
}

pub struct OutlineView {
    page: Entity<Page>,
    scope: OutlineScope,
    /// Fallback focus target (e.g. an empty outline); a block exiting edit
    /// mode parks focus on the block itself (#42), not here.
    focus_handle: FocusHandle,
    blocks: HashMap<BlockId, Entity<BlockView>>,
    /// One subscription per cached `BlockView`, used for sibling-insertion
    /// focus and vertical edit transfers. Pruned alongside `blocks` when ids
    /// disappear from the outline.
    block_subs: HashMap<BlockId, Subscription>,
    /// Attached to the scrolling root so keyboard focus moves can reveal the
    /// target row. Child indices line up with `flatten_visible` order.
    scroll: ScrollHandle,
    /// Built on first render (not in `new`: `Page::new` constructs its view
    /// from `cx.entity()` before the page itself exists). `Page` scope only.
    linked_refs: Option<Entity<LinkedRefsView>>,
    bullet_menu: Option<BulletMenu>,
    /// The block the last render threaded to, so tests can see what was drawn.
    #[cfg(test)]
    rendered_active: Option<BlockId>,
    _page_sub: Subscription,
}

impl OutlineView {
    pub fn new(page: Entity<Page>, cx: &mut Context<Self>) -> Self {
        Self::scoped(page, OutlineScope::Page, cx)
    }

    /// A view over `root` and its descendants only. Edits, indents and moves
    /// still act on `page`, so a linked reference is editable in place.
    pub fn subtree(page: Entity<Page>, root: BlockId, cx: &mut Context<Self>) -> Self {
        Self::scoped(page, OutlineScope::Subtree(root), cx)
    }

    /// The whole page, rendered inside a parent's scroll container (one day
    /// of the journals list).
    pub fn embedded(page: Entity<Page>, cx: &mut Context<Self>) -> Self {
        Self::scoped(page, OutlineScope::Embedded, cx)
    }

    fn scoped(page: Entity<Page>, scope: OutlineScope, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&page, |_, _, cx| cx.notify());
        Self {
            page,
            scope,
            focus_handle: cx.focus_handle(),
            blocks: HashMap::new(),
            block_subs: HashMap::new(),
            scroll: ScrollHandle::new(),
            linked_refs: None,
            bullet_menu: None,
            #[cfg(test)]
            rendered_active: None,
            _page_sub: sub,
        }
    }

    /// The roots this view renders and navigates within. Keeping every
    /// traversal behind this one accessor is what stops focus moves and
    /// backspace in a linked reference from escaping into the rest of the
    /// source page.
    fn scoped_roots<'a>(&self, outline: &'a Outline) -> &'a [Block] {
        match self.scope {
            OutlineScope::Page | OutlineScope::Embedded => &outline.roots,
            OutlineScope::Subtree(id) => outline
                .path_to(id)
                .map_or(&[][..], |path| block_slice_at(&outline.roots, &path)),
        }
    }

    /// Focus the first root block in the outline, creating its `BlockView` if
    /// needed. An empty outline falls back to this view's own focus handle so
    /// the enclosing `RootView` remains in the key dispatch path.
    pub fn focus_first_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self
            .scoped_roots(self.page.read(cx).outline())
            .first()
            .map(|block| block.id);
        let Some(first_id) = first else {
            window.focus(&self.focus_handle, cx);
            return;
        };
        let bv = self.get_or_create(first_id, window, cx);
        let handle = bv.focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Describes the active page-body focus target for the optional root debug
    /// HUD. `contains_focused` follows the real GPUI focus path, so an active
    /// text input is correctly reported as belonging to its block.
    #[must_use]
    pub fn debug_focus_status(&self, window: &Window, cx: &App) -> (&'static str, &'static str) {
        for block in self.blocks.values() {
            let block = block.read(cx);
            if block.focus_handle(cx).contains_focused(window, cx) {
                return if block.is_editing() {
                    ("block input", "editing")
                } else {
                    ("block", "viewing")
                };
            }
        }
        if self.focus_handle.is_focused(window) {
            ("outline", "no block")
        } else {
            ("none", "no block")
        }
    }

    fn get_or_create(
        &mut self,
        id: BlockId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<BlockView> {
        if let Some(existing) = self.blocks.get(&id) {
            return existing.clone();
        }
        let page = self.page.clone();
        let bv = cx.new(|block_cx| BlockView::new(id, page, window, block_cx));
        let sub = cx.subscribe_in(
            &bv,
            window,
            move |this, _bv, event, window, cx| match event {
                BlockViewEvent::FocusRequested(target) => {
                    let target = *target;
                    let target_bv = this.get_or_create(target, window, cx);
                    // Focus no longer implies editing (#42), so the new sibling
                    // must be put into editing explicitly — the user pressed
                    // Enter to keep typing. `begin_editing` focuses the input.
                    target_bv.update(cx, |bv, cx| bv.begin_editing(window, cx));
                }
                BlockViewEvent::AdjacentEditRequested(request) => {
                    this.move_edit_to_adjacent(id, *request, window, cx);
                }
            },
        );
        self.blocks.insert(id, bv.clone());
        self.block_subs.insert(id, sub);
        bv
    }

    fn move_edit_to_adjacent(
        &mut self,
        source: BlockId,
        request: crate::block_view::AdjacentEditRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let flat = flatten_visible(self.scoped_roots(self.page.read(cx).outline()));
        let Some(source_index) = flat.iter().position(|&(_, id)| id == source) else {
            return;
        };
        let target_index = match request.direction {
            VerticalDirection::Up => source_index.checked_sub(1),
            VerticalDirection::Down => (source_index + 1 < flat.len()).then_some(source_index + 1),
        };
        let Some(target_index) = target_index else {
            // Clamp at the first and last outline entries without blurring
            // the source editor or wrapping around.
            return;
        };
        let target = flat[target_index].1;
        let placement =
            PendingVerticalPlacement::for_navigation(request.direction, request.preferred_screen_x);
        let target_bv = self.get_or_create(target, window, cx);
        target_bv.update(cx, |block, cx| {
            block.begin_editing_at(placement, window, cx);
        });
        self.scroll.scroll_to_item(target_index);
    }

    /// The id of the block whose wrapper currently holds focus (i.e. the
    /// focused-but-viewing block the outline keys operate on).
    fn focused_block_id(&self, window: &Window, cx: &App) -> Option<BlockId> {
        self.blocks.iter().find_map(|(id, bv)| {
            bv.read(cx)
                .focus_handle(cx)
                .is_focused(window)
                .then_some(*id)
        })
    }

    /// The block holding focus, whether viewing or editing — the end of the
    /// highlighted thread.
    fn active_block_id(&self, window: &Window, cx: &App) -> Option<BlockId> {
        self.blocks
            .iter()
            .find_map(|(id, bv)| bv.read(cx).holds_focus(window, cx).then_some(*id))
    }

    /// Move focus `delta` steps through the visible (collapse-respecting)
    /// flattened order. Clamps at both ends.
    fn move_focus(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.focused_block_id(window, cx) else {
            return;
        };
        let flat = flatten_visible(self.scoped_roots(self.page.read(cx).outline()));
        let Some(idx) = flat.iter().position(|&(_, id)| id == current) else {
            return;
        };
        let Some(new_idx) = idx.checked_add_signed(delta).filter(|i| *i < flat.len()) else {
            return;
        };
        let (_, target) = flat[new_idx];
        let bv = self.get_or_create(target, window, cx);
        let handle = bv.focus_handle(cx);
        window.focus(&handle, cx);
        self.scroll.scroll_to_item(new_idx);
    }

    /// Fold or unfold `id` from its arrow. Folding away the block that holds
    /// focus would leave focus on a row that is no longer drawn, so it moves
    /// to the folded block.
    fn toggle_fold(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        self.page
            .update(cx, |page, cx| page.toggle_collapse(id, cx));
        let Some(active) = self.active_block_id(window, cx) else {
            return;
        };
        let visible = flatten_visible(self.scoped_roots(self.page.read(cx).outline()));
        if !visible.iter().any(|&(_, v)| v == active) {
            let handle = self.get_or_create(id, window, cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Run one of the `Page` outline ops on the focused block. Focus stays
    /// on the same block — its `BlockView` survives the re-render because
    /// the ops never change block ids.
    fn update_focused_block(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut Page, BlockId, &mut Context<Page>) -> bool,
    ) {
        let Some(current) = self.focused_block_id(window, cx) else {
            return;
        };
        self.page.update(cx, |page, cx| {
            op(page, current, cx);
        });
    }

    /// Indent/outdent apply to the focused block, or to the block being
    /// edited when its caret sits at the very start.
    fn update_indent_target(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut Page, BlockId, &mut Context<Page>) -> bool,
    ) {
        let target = self.focused_block_id(window, cx).or_else(|| {
            self.blocks.iter().find_map(|(id, bv)| {
                let bv = bv.read(cx);
                (bv.holds_focus(window, cx) && bv.caret_at_start(cx)).then_some(*id)
            })
        });
        let Some(target) = target else {
            return;
        };
        self.page.update(cx, |page, cx| {
            op(page, target, cx);
        });
    }

    /// Deletes the focused block with its subtree; see `remove_block`.
    fn delete_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(current) = self.focused_block_id(window, cx) {
            self.remove_block(current, window, cx);
        }
    }

    /// Deletes `current` with its subtree and moves focus to the previous
    /// visible block (or, for the first, the next one after the subtree).
    /// Does nothing when no block would be left to focus.
    fn remove_block(&mut self, current: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        let flat = flatten_visible(self.scoped_roots(self.page.read(cx).outline()));
        let Some(idx) = flat.iter().position(|&(_, id)| id == current) else {
            return;
        };
        let depth = flat[idx].0;
        let after_subtree = flat[idx + 1..]
            .iter()
            .find(|&&(d, _)| d <= depth)
            .map(|&(_, id)| id);
        let Some(neighbor) = idx.checked_sub(1).map(|i| flat[i].1).or(after_subtree) else {
            return;
        };
        self.page.update(cx, |page, cx| {
            page.delete_block(current, cx);
        });
        let handle = self.get_or_create(neighbor, window, cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Puts `id` and its descendants on the clipboard as markdown.
    fn copy_block(&self, id: BlockId, cx: &mut Context<Self>) {
        if let Some(markdown) = self.page.read(cx).outline().subtree_markdown(id) {
            cx.write_to_clipboard(ClipboardItem::new_string(markdown));
        }
    }

    /// Copies `id` with its descendants, then deletes them (undoable like a
    /// Backspace delete).
    fn cut_block(&mut self, id: BlockId, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_block(id, cx);
        self.remove_block(id, window, cx);
    }

    fn open_bullet_menu(
        &mut self,
        block: BlockId,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Don't let the outline's `track_focus` take focus from a block that
        // is being edited.
        window.prevent_default();
        cx.stop_propagation();
        self.bullet_menu = Some(BulletMenu {
            block,
            position: event.position,
        });
        cx.notify();
    }

    /// The bullet's Copy/Cut menu, if one is open. Any mouse-down outside it
    /// closes it.
    fn render_bullet_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.bullet_menu.as_ref()?;
        let id = menu.block;
        let item = |label: &'static str| {
            div()
                .id(label)
                .debug_selector(move || format!("bullet-menu-{label}"))
                .cursor_pointer()
                .px_2()
                .py_0p5()
                .rounded_sm()
                .text_color(theme::header_fg())
                .hover(|s| s.bg(theme::row_hover()))
                .child(label)
        };
        let list = div()
            .id("bullet-menu")
            .flex()
            .flex_col()
            .gap_0p5()
            .bg(theme::overlay_bg())
            .border_1()
            .border_color(theme::overlay_border())
            .rounded_md()
            .p_1()
            .min_w(px(120.0))
            // Keep clicks on the menu from reaching the blocks it covers.
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.bullet_menu = None;
                cx.notify();
            }))
            .child(item("Copy").on_click(cx.listener(move |this, _, _, cx| {
                this.bullet_menu = None;
                this.copy_block(id, cx);
                cx.notify();
            })))
            .child(
                item("Cut").on_click(cx.listener(move |this, _, window, cx| {
                    this.bullet_menu = None;
                    this.cut_block(id, window, cx);
                    cx.notify();
                })),
            );
        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.0))
                    .child(list),
            )
            // Paint above the rows, and outside the outline's scroll container.
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Restores the page's most recently deleted block and focuses it.
    fn undo_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(restored) = self.page.update(cx, Page::undo_delete) else {
            return;
        };
        let handle = self.get_or_create(restored, window, cx).focus_handle(cx);
        window.focus(&handle, cx);
        let flat = flatten_visible(self.scoped_roots(self.page.read(cx).outline()));
        if let Some(idx) = flat.iter().position(|&(_, id)| id == restored) {
            self.scroll.scroll_to_item(idx);
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn block_view(&self, id: BlockId) -> Option<&Entity<BlockView>> {
        self.blocks.get(&id)
    }

    /// How many block rows this view has mounted, i.e. how much of the outline
    /// is on screen. Pruned to the scope on every render.
    #[cfg(test)]
    #[must_use]
    pub fn rendered_block_count(&self) -> usize {
        self.blocks.len()
    }

    /// The linked-references section, once it has been rendered at least once.
    #[must_use]
    pub fn linked_refs(&self) -> Option<&Entity<LinkedRefsView>> {
        self.linked_refs.as_ref()
    }
}

/// The one-element slice holding the block at `path`, so a subtree can be
/// handed to the same `&[Block]` traversals the whole outline uses.
fn block_slice_at<'a>(roots: &'a [Block], path: &[usize]) -> &'a [Block] {
    let Some((&last, parents)) = path.split_last() else {
        return &[];
    };
    let mut blocks = roots;
    for &i in parents {
        blocks = &blocks[i].children;
    }
    &blocks[last..=last]
}

fn flatten_visible(roots: &[Block]) -> Vec<(usize, BlockId)> {
    fn walk(blocks: &[Block], depth: usize, out: &mut Vec<(usize, BlockId)>) {
        for b in blocks {
            out.push((depth, b.id));
            if !b.collapsed {
                walk(&b.children, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(roots, 0, &mut out);
    out
}

/// What one row draws of the thread that joins the active block to each of
/// its ancestors, Logseq-style: down from the parent's bullet, through any
/// rows in between, then a curve into the child's bullet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RowThread {
    /// This block is the active one or one of its ancestors.
    on_path: bool,
    /// The thread comes down its parent's column and turns into this bullet.
    enters: bool,
    /// The thread leaves this bullet downward, toward a child on the path.
    leaves: bool,
    /// The thread passes straight through this row in that depth's column.
    passes: Option<usize>,
}

/// One `RowThread` per entry of `flat` (a `flatten_visible` result). All
/// default when `active` is `None` or not on screen.
fn thread_rows(flat: &[(usize, BlockId)], active: Option<BlockId>) -> Vec<RowThread> {
    let mut rows = vec![RowThread::default(); flat.len()];
    let Some(active) = active.and_then(|a| flat.iter().position(|&(_, id)| id == a)) else {
        return rows;
    };
    rows[active].on_path = true;
    // Walking back from a block, the first shallower row is its parent;
    // everything before that is a sibling's subtree the thread runs past.
    let mut child = active;
    for row in (0..active).rev() {
        let depth = flat[child].0;
        if depth == 0 {
            break;
        }
        if flat[row].0 < depth {
            rows[child].enters = true;
            rows[row].leaves = true;
            rows[row].on_path = true;
            child = row;
        } else {
            rows[row].passes = Some(depth - 1);
        }
    }
    rows
}

/// The x of the bullet centre for blocks at `depth`, from the row's left edge.
#[allow(clippy::cast_precision_loss)]
fn bullet_x(depth: usize) -> Pixels {
    INDENT_PER_LEVEL * depth as f32 + BULLET_COLUMN_WIDTH / 2.
}

/// A 1px vertical line centred on `depth`'s bullet column, from `top` to the
/// row's bottom — and on across the gap into the next row.
fn vertical_line(depth: usize, top: Pixels, color: gpui::Rgba) -> gpui::Div {
    div()
        .absolute()
        .left(bullet_x(depth) - px(0.5))
        .top(top)
        .bottom(-ROW_GAP)
        .w(px(1.0))
        .bg(color)
}

/// Whether a row's block has children, and if so whether they are folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fold {
    Leaf,
    Open,
    Folded,
}

impl Fold {
    fn of(block: &Block) -> Self {
        match (block.children.is_empty(), block.collapsed) {
            (true, _) => Self::Leaf,
            (false, false) => Self::Open,
            (false, true) => Self::Folded,
        }
    }
}

/// One outline row: indent, the fold arrow, the bullet with its guides and
/// thread, and the block itself.
fn thread_row(
    depth: usize,
    thread: RowThread,
    fold: Fold,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_bullet_menu: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    bv: Entity<BlockView>,
) -> gpui::Div {
    #[allow(clippy::cast_precision_loss)]
    let indent = INDENT_PER_LEVEL * depth as f32;
    // Faint guides down every ancestor's column, then the active
    // thread over them. Both paint before the bullet so it covers
    // their ends.
    let guides = (0..depth).map(|d| vertical_line(d, px(0.), theme::thread_guide()));
    let passes = thread
        .passes
        .map(|d| vertical_line(d, px(0.), theme::accent()));
    let leaves = thread
        .leaves
        .then(|| vertical_line(depth, px(BULLET_CENTER_Y), theme::accent()));
    let enters = thread.enters.then(|| {
        // An L from the parent's column into this bullet: its left and
        // bottom borders, rounded at the corner, the bottom border
        // centred on the bullet.
        div()
            .absolute()
            .left(bullet_x(depth - 1) - px(0.5))
            .top(-ROW_GAP)
            .w(INDENT_PER_LEVEL - px(BULLET_SIZE / 2.))
            .h(ROW_GAP + px(BULLET_CENTER_Y + 0.5))
            .border_l_1()
            .border_b_1()
            .border_color(theme::accent())
            .rounded_bl(THREAD_RADIUS)
    });
    let bullet_color = if thread.on_path {
        theme::accent()
    } else {
        theme::fg_muted()
    };
    let row_id = bv.entity_id();
    // Hidden until the row (or the arrow itself, which hangs left of a root
    // row's bounds) is hovered.
    let toggle = (fold != Fold::Leaf).then(|| {
        div()
            .id(("fold-toggle", row_id))
            .debug_selector(move || format!("fold-toggle-{row_id}"))
            .absolute()
            .left(indent - FOLD_TOGGLE_WIDTH)
            .top(px(BULLET_CENTER_Y - FOLDED_RING_SIZE / 2.))
            .w(FOLD_TOGGLE_WIDTH)
            .h(px(FOLDED_RING_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(theme::fg_muted())
            .cursor_pointer()
            .opacity(0.)
            .group_hover(ROW_GROUP, |s| s.opacity(1.))
            .hover(|s| s.opacity(1.).text_color(theme::fg()))
            // Don't let the outline's `track_focus` take focus from a block
            // that is being edited.
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                window.prevent_default();
            })
            .on_click(on_toggle)
            .child(if fold == Fold::Folded { "▸" } else { "▾" })
    });
    let ring = if fold == Fold::Folded {
        theme::folded_bullet_ring()
    } else {
        theme::transparent()
    };
    div()
        .group(ROW_GROUP)
        .relative()
        .flex()
        .flex_row()
        .w_full()
        .max_w(theme::CONTENT_MAX_WIDTH)
        .mx_auto()
        .min_w_0()
        .items_start()
        .pl(indent)
        .children(guides)
        .children(passes)
        .children(leaves)
        .children(enters)
        .children(toggle)
        .child(
            div()
                .id(("bullet", row_id))
                .debug_selector(move || format!("bullet-{row_id}"))
                .on_mouse_down(MouseButton::Right, on_bullet_menu)
                .w(BULLET_COLUMN_WIDTH)
                .flex_none()
                .flex()
                .justify_center()
                .pt(px(BULLET_CENTER_Y - FOLDED_RING_SIZE / 2.))
                .child(
                    div()
                        .size(px(FOLDED_RING_SIZE))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(ring)
                        .child(div().size(px(BULLET_SIZE)).rounded_full().bg(bullet_color)),
                ),
        )
        .child(bv)
}

fn collect_folds(blocks: &[Block], out: &mut HashMap<BlockId, Fold>) {
    for b in blocks {
        out.insert(b.id, Fold::of(b));
        collect_folds(&b.children, out);
    }
}

impl Render for OutlineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (flat, folds) = {
            let roots = self.scoped_roots(self.page.read(cx).outline());
            let mut folds = HashMap::new();
            collect_folds(roots, &mut folds);
            (flatten_visible(roots), folds)
        };
        self.blocks.retain(|id, _| folds.contains_key(id));
        self.block_subs.retain(|id, _| folds.contains_key(id));

        // The bindings target the `BlockView` key context (see BLOCK_CONTEXT),
        // but the handlers live here because the ops need the outline-wide
        // view map and traversal order. Actions dispatched from a focused
        // block bubble up through this ancestor node.
        let root = div()
            .id("outline")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
                this.move_focus(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.move_focus(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &IndentBlock, window, cx| {
                this.update_indent_target(window, cx, Page::indent_block);
            }))
            .on_action(cx.listener(|this, _: &OutdentBlock, window, cx| {
                this.update_indent_target(window, cx, Page::outdent_block);
            }))
            .on_action(cx.listener(|this, _: &MoveBlockUp, window, cx| {
                this.update_focused_block(window, cx, Page::move_block_up);
            }))
            .on_action(cx.listener(|this, _: &MoveBlockDown, window, cx| {
                this.update_focused_block(window, cx, Page::move_block_down);
            }))
            .on_action(cx.listener(|this, _: &ToggleCollapse, window, cx| {
                this.update_focused_block(window, cx, Page::toggle_collapse);
            }))
            .on_action(cx.listener(|this, _: &DeleteBlock, window, cx| {
                this.delete_block(window, cx);
            }))
            .on_action(cx.listener(|this, _: &UndoDelete, window, cx| {
                this.undo_delete(window, cx);
            }))
            .flex()
            .flex_col()
            .w_full()
            .line_height(relative(LINE_HEIGHT))
            .gap(ROW_GAP);

        // Only the page-level view scrolls: a subtree view is rendered inside
        // that scroll container, and nesting a second one would trap the wheel.
        let mut root = match self.scope {
            OutlineScope::Page => root.flex_1().min_h_0().overflow_y_scroll().track_scroll(
                // Child indices line up with `flat`, so the refs section must
                // stay the last child (see below).
                &self.scroll,
            ),
            OutlineScope::Embedded | OutlineScope::Subtree(_) => root,
        };

        let active = self.active_block_id(window, cx);
        #[cfg(test)]
        {
            self.rendered_active = active;
        }
        let threads = thread_rows(&flat, active);
        for ((depth, id), thread) in flat.into_iter().zip(threads) {
            let bv = self.get_or_create(id, window, cx);
            let on_toggle =
                cx.listener(move |this, _, window, cx| this.toggle_fold(id, window, cx));
            let on_bullet_menu = cx.listener(move |this, event, window, cx| {
                this.open_bullet_menu(id, event, window, cx);
            });
            root = root.child(thread_row(
                depth,
                thread,
                folds[&id],
                on_toggle,
                on_bullet_menu,
                bv,
            ));
        }

        if self.scope == OutlineScope::Page {
            let page = self.page.clone();
            let refs = self
                .linked_refs
                .get_or_insert_with(|| cx.new(|cx| LinkedRefsView::new(page, cx)));
            root = root.child(
                div()
                    .w_full()
                    .max_w(theme::CONTENT_MAX_WIDTH)
                    .mx_auto()
                    .child(refs.clone()),
            );
        }
        // After the rows (and refs), so it doesn't shift `scroll_to_item`
        // indices.
        root.children(self.render_bullet_menu(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::Block;
    use crate::text_input;
    use gpui::{point, size, Modifiers, TestAppContext, VisualTestContext};

    struct TestPage(Entity<Page>);
    impl gpui::Global for TestPage {}

    /// `flatten_visible` rows of an outline with blocks at these depths.
    fn flat_of(depths: &[usize]) -> Vec<(usize, BlockId)> {
        let mut body = String::new();
        for &d in depths {
            body.push_str(&"\t".repeat(d));
            body.push_str("- x\n");
        }
        let flat = flatten_visible(&Outline::parse(&body).roots);
        assert_eq!(flat.iter().map(|&(d, _)| d).collect::<Vec<_>>(), depths);
        flat
    }

    #[test]
    fn thread_joins_each_ancestor_to_the_next_through_rows_between() {
        // a / a1 / a1x / a2 / a2y — active a2y.
        let flat = flat_of(&[0, 1, 2, 1, 2]);
        let rows = thread_rows(&flat, Some(flat[4].1));
        let none = RowThread::default();
        assert_eq!(
            rows,
            vec![
                RowThread {
                    on_path: true,
                    leaves: true,
                    ..none
                },
                // a1's subtree sits between a and a2: the thread runs past
                // it in a's column.
                RowThread {
                    passes: Some(0),
                    ..none
                },
                RowThread {
                    passes: Some(0),
                    ..none
                },
                RowThread {
                    on_path: true,
                    enters: true,
                    leaves: true,
                    ..none
                },
                RowThread {
                    on_path: true,
                    enters: true,
                    ..none
                },
            ]
        );
    }

    #[test]
    fn thread_stops_at_a_root_and_ignores_later_rows() {
        // a / b / b1 / c — active b: a root, so only its bullet lights up.
        let flat = flat_of(&[0, 0, 1, 0]);
        let rows = thread_rows(&flat, Some(flat[1].1));
        let none = RowThread::default();
        assert_eq!(
            rows,
            vec![
                none,
                RowThread {
                    on_path: true,
                    ..none
                },
                none,
                none
            ]
        );
    }

    #[test]
    fn no_thread_without_an_active_block() {
        let flat = flat_of(&[0, 1]);
        assert_eq!(thread_rows(&flat, None), vec![RowThread::default(); 2]);
    }

    /// Mount an `OutlineView` for `body` as the window root and activate the
    /// window. Same shape as `block_view::tests::mount` — focus and event
    /// dispatch only fire on active windows during `Window::draw`.
    fn mount<'a>(
        cx: &'a mut TestAppContext,
        body: &str,
    ) -> (Entity<Page>, Entity<OutlineView>, &'a mut VisualTestContext) {
        let body = body.to_string();
        let (ov, vcx) = cx.add_window_view(move |_window, cx| {
            text_input::bind_keys(cx);
            crate::block_view::bind_keys(cx);
            super::bind_keys(cx);
            let page = cx.new(|cx| Page::new("foo".into(), &body, cx));
            cx.set_global(TestPage(page));
            OutlineView::new(cx.global::<TestPage>().0.clone(), cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        let page = vcx.read(|cx| cx.global::<TestPage>().0.clone());
        (page, ov, vcx)
    }

    /// Same as `mount`, but the view is scoped to the first root's subtree —
    /// the shape the linked-references rows use.
    fn mount_subtree<'a>(
        cx: &'a mut TestAppContext,
        body: &str,
    ) -> (Entity<Page>, Entity<OutlineView>, &'a mut VisualTestContext) {
        let body = body.to_string();
        let (ov, vcx) = cx.add_window_view(move |_window, cx| {
            text_input::bind_keys(cx);
            crate::block_view::bind_keys(cx);
            super::bind_keys(cx);
            let page = cx.new(|cx| Page::new("foo".into(), &body, cx));
            let root = page.read(cx).outline().roots[0].id;
            cx.set_global(TestPage(page));
            OutlineView::subtree(cx.global::<TestPage>().0.clone(), root, cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        let page = vcx.read(|cx| cx.global::<TestPage>().0.clone());
        (page, ov, vcx)
    }

    #[gpui::test]
    fn subtree_scope_renders_only_that_block_and_its_descendants(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount_subtree(cx, "- a\n\t- a1\n- b\n");

        let (a, a1, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (
                outline.roots[0].id,
                outline.roots[0].children[0].id,
                outline.roots[1].id,
            )
        });

        cx.read(|cx| {
            let view = ov.read(cx);
            assert!(view.block_view(a).is_some(), "root of the subtree renders");
            assert!(view.block_view(a1).is_some(), "descendants render");
            assert!(
                view.block_view(b).is_none(),
                "a sibling outside the subtree must not render",
            );
        });
    }

    #[gpui::test]
    fn subtree_focus_clamps_at_its_own_last_block(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount_subtree(cx, "- a\n\t- a1\n- b\n");

        let a1 = cx.read(|cx| page.read(cx).outline().roots[0].children[0].id);

        // Focus is precondition setup here; the behaviour under test is the key.
        cx.update(|window, cx| {
            let bv = ov
                .read(cx)
                .block_view(a1)
                .cloned()
                .expect("descendant rendered");
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("down");
        cx.run_until_parked();

        cx.update(|window, cx| {
            let bv = ov.read(cx).block_view(a1).cloned().expect("still cached");
            assert!(
                bv.read(cx).focus_handle(cx).is_focused(window),
                "down must not escape the subtree into the next root block",
            );
        });
    }

    #[gpui::test]
    fn enter_creates_sibling_at_same_depth(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");

        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);

        cx.update(|window, cx| {
            let bv = ov
                .read(cx)
                .block_view(first_id)
                .cloned()
                .unwrap_or_else(|| ov.update(cx, |o, cx| o.get_or_create(first_id, window, cx)));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        // First enter begins editing the focused block (#42); the second,
        // now dispatched to the TextInput, submits and creates the sibling.
        cx.simulate_keystrokes("enter enter");
        cx.run_until_parked();

        // Outline now has 3 roots in order: a, <new>, b.
        let (roots, new_id) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            let roots: Vec<(BlockId, String)> = outline
                .roots
                .iter()
                .map(|b| (b.id, b.text.clone()))
                .collect();
            let new_id = outline.roots[1].id;
            (roots, new_id)
        });
        assert_eq!(roots.len(), 3, "expected new sibling inserted: {roots:?}");
        assert_eq!(roots[0].1, "a");
        assert_eq!(roots[1].1, "");
        assert_eq!(roots[2].1, "b");

        // The new block's BlockView should be mounted, editing, and focused.
        cx.update(|window, cx| {
            let new_bv = ov
                .read(cx)
                .block_view(new_id)
                .expect("new block view mounted")
                .clone();
            assert!(new_bv.read(cx).is_editing(), "new block should be editing");
            // Focus advanced to the mounted TextInput, not the wrapper handle.
            let input_focus = new_bv
                .read(cx)
                .input_focus_handle_for_test(cx)
                .expect("input mounted");
            assert!(
                input_focus.is_focused(window),
                "new block's TextInput should hold focus",
            );
        });

        assert!(cx.read(|cx| page.read(cx).dirty()));
    }

    #[gpui::test]
    fn new_block_edits_flush_back_to_outline(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n");

        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);
        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(first_id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter enter");
        cx.run_until_parked();

        // Write into the new block's TextInput directly, then drive the blur
        // path that mounts back to the outline. simulate_input would route
        // through the dispatch tree, which depends on draw timing this test
        // doesn't want to assert.
        let new_id = cx.read(|cx| page.read(cx).outline().roots[1].id);
        cx.update(|window, cx| {
            let new_bv = ov
                .read(cx)
                .block_view(new_id)
                .expect("new block mounted")
                .clone();
            let input = new_bv
                .read(cx)
                .input_entity_for_test()
                .expect("input mounted");
            input.update(cx, |i, cx| i.test_replace_all("hello", cx));
            new_bv.update(cx, |b, cx| b.test_end_editing(window, cx));
        });

        cx.read(|cx| {
            assert_eq!(page.read(cx).outline().get(new_id), Some("hello"));
        });
    }

    #[gpui::test]
    fn enter_on_nested_block_creates_sibling_at_same_depth(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - child\n");

        let child_id = cx.read(|cx| page.read(cx).outline().roots[0].children[0].id);
        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(child_id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter enter");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 1, "still one root");
            let children = &outline.roots[0].children;
            assert_eq!(children.len(), 2, "child got a sibling");
            assert_eq!(children[0].text, "child");
            assert_eq!(children[1].text, "");
        });
    }

    /// Editing → Viewing via blur. Realized as "focus another block": when
    /// block 2 takes focus, block 1's input loses focus, its `on_blur`
    /// listener fires, and block 1 flushes back to view mode.
    ///
    /// Done through `OutlineView` rather than `BlockView` directly because
    /// `BlockView` test setups have no second focusable element to receive
    /// focus, and `Window::blur` alone does not drive `on_blur` reliably
    /// in the headless dispatch.
    #[gpui::test]
    fn transition_editing_to_viewing_via_blur_across_blocks(cx: &mut TestAppContext) {
        use crate::block_view::BlockMode;
        let (page, ov, cx) = mount(cx, "- one\n- two\n");

        let (first_id, second_id) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });

        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(first_id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();
        // Focus no longer implies editing (#42) — enter starts the edit.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.update(|window, cx| {
            // Force a draw so the input's focus_id is captured in the
            // rendered_frame focus path. Without this, the next focus
            // change can't be detected as a blur — the focus listener
            // compares against the rendered_frame's path.
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let bv = ov.read(cx).block_view(first_id).expect("first mounted");
            assert!(bv.read(cx).is_editing(), "precondition: first is editing");
        });

        // Move focus to the second block; this naturally blurs the first
        // block's TextInput.
        cx.update(|window, cx| {
            let other = ov.update(cx, |o, cx| o.get_or_create(second_id, window, cx));
            window.focus(&other.focus_handle(cx), cx);
            window.draw(cx).clear();
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let bv = ov
                .read(cx)
                .block_view(first_id)
                .expect("first still cached");
            assert!(
                matches!(bv.read(cx).mode(), BlockMode::Viewing),
                "blurring the first block should exit its edit mode",
            );
        });
    }

    /// The page-switch mechanism for #42: `RootView::focus_current` lands on
    /// `focus_first_block`, which must leave the block focused-but-viewing —
    /// no `TextInput` mounts and the page cannot be touched by the switch.
    #[gpui::test]
    fn focus_first_block_does_not_mount_editor(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");

        cx.update(|window, cx| {
            ov.update(cx, |o, cx| o.focus_first_block(window, cx));
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            let first_id = page.read(cx).outline().roots[0].id;
            let bv = ov
                .read(cx)
                .block_view(first_id)
                .expect("first block view cached")
                .clone();
            assert!(
                !bv.read(cx).is_editing(),
                "focus_first_block must not mount an editor (#42)",
            );
            assert!(
                bv.read(cx).focus_handle(cx).is_focused(window),
                "first block holds focus in viewing mode",
            );
        });
    }

    /// Focuses the block with `id` (mounting its view if needed) and settles.
    fn focus_block(cx: &mut VisualTestContext, ov: &Entity<OutlineView>, id: BlockId) {
        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();
    }

    fn focused_id(cx: &mut VisualTestContext, ov: &Entity<OutlineView>) -> Option<BlockId> {
        cx.update(|window, cx| ov.read(cx).focused_block_id(window, cx))
    }

    fn editing_input(
        cx: &mut VisualTestContext,
        ov: &Entity<OutlineView>,
        id: BlockId,
    ) -> Entity<crate::text_input::TextInput> {
        cx.read(|cx| {
            ov.read(cx)
                .block_view(id)
                .expect("block view mounted")
                .read(cx)
                .input_entity_for_test()
                .expect("block should be editing")
        })
    }

    #[gpui::test]
    fn rows_match_logseq_vertical_rhythm(cx: &mut TestAppContext) {
        let (page, _ov, cx) = mount(cx, "- a\n- b\n");
        cx.run_until_parked();
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });
        let mut top = |id: BlockId| {
            // `debug_bounds` wants a `'static` name; leaking one is fine in a test.
            let name: &'static str = format!("block-view-{id:?}").leak();
            cx.debug_bounds(name).expect("block painted").top()
        };
        let pitch = top(b) - top(a);
        assert_eq!(pitch, px(31.), "Logseq steps single-line rows 31px apart");
    }

    // ── #44: keyboard navigation and block operations ──────────────────

    #[gpui::test]
    fn up_down_move_focus_through_visible_blocks(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });

        focus_block(cx, &ov, a);

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(focused_id(cx, &ov), Some(b), "down moves to next block");

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(
            focused_id(cx, &ov),
            Some(b),
            "down clamps at the last block"
        );

        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert_eq!(focused_id(cx, &ov), Some(a), "up moves back to the first");

        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert_eq!(focused_id(cx, &ov), Some(a), "up clamps at the first block");
    }

    #[gpui::test]
    fn vertical_arrows_move_visual_rows_and_collapse_active_selection(cx: &mut TestAppContext) {
        let body = format!("- {}\n", "alpha beta gamma delta ".repeat(12));
        let (page, ov, cx) = mount(cx, &body);
        cx.simulate_resize(size(px(240.), px(600.)));
        cx.run_until_parked();
        let block = cx.read(|cx| page.read(cx).outline().roots[0].id);

        focus_block(cx, &ov, block);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let input = editing_input(cx, &ov, block);
        let (row_count, input_bounds) = cx.read(|cx| {
            let input = input.read(cx);
            (
                input.visual_row_count_for_test(),
                input.layout_bounds_for_test(),
            )
        });
        assert!(
            row_count.is_some_and(|rows| rows > 2),
            "precondition: editor wraps to several visual rows; \
             rows={row_count:?}, bounds={input_bounds:?}",
        );

        cx.simulate_keystrokes("home shift-right shift-right down");
        cx.run_until_parked();
        cx.read(|cx| {
            let input = input.read(cx);
            assert!(
                input.selected_range().is_empty(),
                "plain Down clears an active selection",
            );
            assert_eq!(
                input.visual_row_for_test(),
                Some(1),
                "movement continues from the active selection head",
            );
        });

        let before_shifted = cx.read(|cx| {
            let input = input.read(cx);
            (input.cursor_offset_for_test(), input.visual_row_for_test())
        });
        cx.simulate_keystrokes("shift-down shift-up");
        cx.run_until_parked();
        cx.read(|cx| {
            let input = input.read(cx);
            assert_eq!(
                (input.cursor_offset_for_test(), input.visual_row_for_test(),),
                before_shifted,
                "Shift-Up/Down remain unbound in block editors",
            );
        });
    }

    #[gpui::test]
    #[allow(clippy::too_many_lines)] // One end-to-end scenario verifies sticky x before, during, and after transfer.
    fn vertical_arrows_cross_blocks_and_preserve_sticky_screen_column(cx: &mut TestAppContext) {
        let root_text = format!("{}alpha beta x", "alpha beta gamma delta ".repeat(9));
        let child_text = "one two three four five six ".repeat(10);
        let body = format!("- {root_text}\n  - {child_text}\n");
        let (page, ov, cx) = mount(cx, &body);
        cx.simulate_resize(size(px(280.), px(700.)));
        cx.run_until_parked();
        let (root, child) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[0].children[0].id)
        });

        focus_block(cx, &ov, root);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let root_input = editing_input(cx, &ov, root);
        let (click, rows) = cx.read(|cx| {
            let input = root_input.read(cx);
            let bounds = input.layout_bounds_for_test().expect("root layout");
            let line_height = input.line_height_for_test().expect("line height");
            (
                point(bounds.left() + px(200.), bounds.top() + line_height / 2.),
                input.visual_row_count_for_test().expect("row count"),
            )
        });
        assert!(rows > 2, "precondition: root wraps");
        cx.simulate_click(click, Modifiers::default());
        cx.run_until_parked();
        let preferred_x = cx.read(|cx| {
            root_input
                .read(cx)
                .cursor_screen_position_for_test()
                .expect("cursor position")
                .x
        });

        for _ in 1..rows {
            cx.simulate_keystrokes("down");
            cx.run_until_parked();
        }
        assert!(
            cx.read(|cx| root_input.read(cx).visual_row_for_test()) == Some(rows - 1),
            "caret should reach the root's last visual row before crossing",
        );
        let clamped_x = cx.read(|cx| {
            root_input
                .read(cx)
                .cursor_screen_position_for_test()
                .expect("cursor on last root row")
                .x
        });
        let (first_width, last_width) = cx.read(|cx| {
            let input = root_input.read(cx);
            (
                input.visual_row_width_for_test(0),
                input.visual_row_width_for_test(rows - 1),
            )
        });
        assert!(
            clamped_x + px(8.) < preferred_x,
            "precondition: the short last row clamps the sticky column; \
             preferred={preferred_x:?}, clamped={clamped_x:?}, \
             first_width={first_width:?}, last_width={last_width:?}",
        );

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        let child_input = editing_input(cx, &ov, child);
        cx.update(|window, cx| {
            let root_view = ov.read(cx).block_view(root).expect("root cached").read(cx);
            let child_view = ov
                .read(cx)
                .block_view(child)
                .expect("child cached")
                .read(cx);
            assert!(!root_view.is_editing(), "source exits edit mode normally");
            assert!(child_view.is_editing(), "destination enters edit mode");
            assert!(
                child_input.focus_handle(cx).is_focused(window),
                "destination TextInput receives focus",
            );
        });
        cx.read(|cx| {
            let input = child_input.read(cx);
            assert_eq!(
                input.visual_row_for_test(),
                Some(0),
                "Down enters the next block on its first visual row",
            );
            let destination_x = input
                .cursor_screen_position_for_test()
                .expect("destination cursor")
                .x;
            assert!(
                (destination_x - preferred_x).abs() <= px(8.),
                "absolute screen column should survive indentation: \
                 preferred={preferred_x:?}, destination={destination_x:?}",
            );
        });

        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        let root_input = editing_input(cx, &ov, root);
        cx.read(|cx| {
            let input = root_input.read(cx);
            assert_eq!(
                input.visual_row_for_test(),
                input
                    .visual_row_count_for_test()
                    .map(|row_count| row_count - 1),
                "Up enters the previous block on its last visual row",
            );
            let destination_x = input
                .cursor_screen_position_for_test()
                .expect("destination cursor")
                .x;
            assert!(
                (destination_x - clamped_x).abs() <= px(8.),
                "Up lands on the sticky column, clamped by the short last row: \
                 clamped={clamped_x:?}, destination={destination_x:?}",
            );
        });
    }

    #[gpui::test]
    fn thread_follows_a_vertical_edit_move_on_the_same_frame(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n\t- b\n\t- c\n");
        let (b, c) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (
                outline.roots[0].children[0].id,
                outline.roots[0].children[1].id,
            )
        });

        focus_block(cx, &ov, c);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| ov.read(cx).rendered_active), Some(c));

        // The move mounts b's editor and focuses it in one go: the render
        // that follows must already thread to b, not wait for another frame.
        cx.simulate_keystrokes("up");
        assert_eq!(cx.read(|cx| ov.read(cx).rendered_active), Some(b));
    }

    #[gpui::test]
    fn vertical_edit_navigation_respects_collapse_and_outline_boundaries(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- parent\n  - hidden\n- next\n");
        let (parent, hidden, next) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (
                outline.roots[0].id,
                outline.roots[0].children[0].id,
                outline.roots[1].id,
            )
        });

        focus_block(cx, &ov, parent);
        cx.simulate_keystrokes(&format!("{}-enter", crate::cmd_key()));
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("home up");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(parent)
                .expect("parent mounted")
                .read(cx)
                .is_editing()),
            "Up clamps at the first visible block without leaving edit mode",
        );

        cx.simulate_keystrokes("end down");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(next)
                .expect("next mounted")
                .read(cx)
                .is_editing()),
            "collapsed descendants must be skipped",
        );
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(hidden)
                .is_none_or(|view| !view.read(cx).is_editing())),
            "hidden child must not receive the edit transfer",
        );

        cx.simulate_keystrokes("end down");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(next)
                .expect("next mounted")
                .read(cx)
                .is_editing()),
            "Down clamps at the last visible block without leaving edit mode",
        );
    }

    #[gpui::test]
    fn tab_indents_focused_block(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[1].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 1, "b indented under a");
            assert_eq!(outline.roots[0].children[0].text, "b");
            assert!(page.read(cx).dirty(), "structural edit dirties the page");
        });
        assert_eq!(focused_id(cx, &ov), Some(b), "focus stays on the block");
    }

    #[gpui::test]
    fn shift_tab_outdents_focused_block(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[0].children[0].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 2, "b raised to root level");
            assert_eq!(outline.roots[1].text, "b");
        });
    }

    #[gpui::test]
    fn alt_up_down_move_block_among_siblings(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let a = cx.read(|cx| page.read(cx).outline().roots[0].id);

        focus_block(cx, &ov, a);
        cx.simulate_keystrokes("alt-down");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots[0].text, "b");
            assert_eq!(outline.roots[1].text, "a");
        });

        cx.simulate_keystrokes("alt-up");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots[0].text, "a", "alt-up moves it back");
        });
    }

    /// Right-clicks `id`'s bullet, then clicks `item` in the menu it opens.
    fn bullet_menu_click(
        cx: &mut VisualTestContext,
        ov: &Entity<OutlineView>,
        id: BlockId,
        item: &str,
    ) {
        let entity = cx.read(|cx| ov.read(cx).block_view(id).expect("row mounted").entity_id());
        let bullet = cx
            .debug_bounds(format!("bullet-{entity}").leak())
            .expect("bullet painted")
            .center();
        cx.simulate_mouse_down(bullet, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(bullet, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        let item = cx
            .debug_bounds(format!("bullet-menu-{item}").leak())
            .expect("menu open")
            .center();
        cx.simulate_click(item, Modifiers::default());
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("bullet-menu-Copy").is_none(),
            "choosing an item closes the menu",
        );
    }

    fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
    }

    #[gpui::test]
    fn bullet_menu_copy_puts_the_subtree_on_the_clipboard(cx: &mut TestAppContext) {
        let body = "- a\n- b\n  - c\n- d\n";
        let (page, ov, cx) = mount(cx, body);
        let b = cx.read(|cx| page.read(cx).outline().roots[1].id);

        bullet_menu_click(cx, &ov, b, "Copy");

        assert_eq!(clipboard(cx).as_deref(), Some("- b\n  - c\n"));
        cx.read(|cx| {
            assert_eq!(
                page.read(cx).outline().serialize(),
                body,
                "copy changes nothing"
            );
            assert!(!page.read(cx).dirty());
        });
    }

    #[gpui::test]
    fn bullet_menu_cut_copies_then_deletes_undoably(cx: &mut TestAppContext) {
        let body = "- a\n  - b\n    - c\n- d\n";
        let (page, ov, cx) = mount(cx, body);
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[0].children[0].id)
        });

        bullet_menu_click(cx, &ov, b, "Cut");

        assert_eq!(clipboard(cx).as_deref(), Some("- b\n  - c\n"));
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- a\n- d\n"));
        assert_eq!(focused_id(cx, &ov), Some(a), "previous block takes focus");

        cx.simulate_keystrokes(&format!("{}-z", crate::cmd_key()));
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), body));
    }

    /// Copy a bullet, then paste into an empty block: the same tree comes
    /// back as blocks, not as one line of markdown.
    #[gpui::test]
    fn pasting_a_copied_bullet_recreates_its_children(cx: &mut TestAppContext) {
        let tree = "- # h1\n  - ## h2\n    - ### h3\n      - #### h4\n";
        let (page, ov, cx) = mount(cx, &format!("{tree}-\n"));
        let (h1, empty) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });

        bullet_menu_click(cx, &ov, h1, "Copy");
        focus_block(cx, &ov, empty);
        cx.simulate_keystrokes("enter");
        cx.simulate_keystrokes(&format!("{}-v", crate::cmd_key()));
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(page.read(cx).outline().serialize(), format!("{tree}{tree}"));
        });
        assert_eq!(
            focused_id(cx, &ov),
            Some(empty),
            "the pasted-into block keeps focus"
        );
        cx.read(|cx| {
            let bv = ov.read(cx).block_view(empty).unwrap();
            assert!(!bv.read(cx).is_editing(), "the paste ends the edit");
        });
    }

    #[gpui::test]
    fn plain_text_still_pastes_into_the_editor(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n");
        let a = cx.read(|cx| page.read(cx).outline().roots[0].id);
        cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("bc".into())));

        focus_block(cx, &ov, a);
        cx.simulate_keystrokes("enter");
        cx.simulate_keystrokes(&format!("{}-v", crate::cmd_key()));
        cx.run_until_parked();

        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- abc\n"));
    }

    #[gpui::test]
    fn clicking_outside_the_bullet_menu_closes_it(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let a = cx.read(|cx| page.read(cx).outline().roots[0].id);
        let entity = cx.read(|cx| ov.read(cx).block_view(a).unwrap().entity_id());
        let bullet = cx
            .debug_bounds(format!("bullet-{entity}").leak())
            .unwrap()
            .center();
        cx.simulate_mouse_down(bullet, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(bullet, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("bullet-menu-Copy").is_some(), "menu open");

        cx.simulate_click(point(px(1.), px(1.)), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("bullet-menu-Copy").is_none(), "menu closed");
    }

    /// Clicks the fold arrow on `id`'s row.
    fn click_fold_toggle(cx: &mut VisualTestContext, ov: &Entity<OutlineView>, id: BlockId) {
        let entity = cx.read(|cx| ov.read(cx).block_view(id).expect("row mounted").entity_id());
        // `debug_bounds` wants a `'static` name; leaking one is fine in a test.
        let name: &'static str = format!("fold-toggle-{entity}").leak();
        let center = cx.debug_bounds(name).expect("fold arrow painted").center();
        cx.simulate_click(center, Modifiers::default());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn clicking_the_fold_arrow_folds_and_unfolds_without_dirtying(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - b\n- c\n");
        cx.run_until_parked();
        let (a, c) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });
        let c_entity = cx.read(|cx| ov.read(cx).block_view(c).unwrap().entity_id());
        assert!(
            cx.debug_bounds(format!("fold-toggle-{c_entity}").leak())
                .is_none(),
            "a childless block has no arrow",
        );

        click_fold_toggle(cx, &ov, a);
        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert!(outline.roots[0].collapsed, "a folded");
            assert_eq!(flatten_visible(&outline.roots).len(), 2, "b hidden");
            assert!(!page.read(cx).dirty(), "folding must not dirty the page");
        });

        click_fold_toggle(cx, &ov, a);
        cx.read(|cx| {
            assert!(!page.read(cx).outline().roots[0].collapsed, "a unfolded");
        });
    }

    #[gpui::test]
    fn folding_away_the_edited_block_moves_focus_to_the_folded_one(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - b\n");
        cx.run_until_parked();
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[0].children[0].id)
        });
        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        editing_input(cx, &ov, b);

        click_fold_toggle(cx, &ov, a);
        assert_eq!(
            focused_id(cx, &ov),
            Some(a),
            "focus lands on the folded block"
        );
        cx.read(|cx| {
            let bv = ov.read(cx).block_view(b).expect("b still cached");
            assert!(!bv.read(cx).is_editing(), "b's editor closed");
        });
    }

    #[gpui::test]
    fn primary_modifier_enter_toggles_collapse_without_dirtying(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - b\n");
        let a = cx.read(|cx| page.read(cx).outline().roots[0].id);
        let shortcut = format!("{}-enter", crate::cmd_key());

        focus_block(cx, &ov, a);
        cx.simulate_keystrokes(&shortcut);
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert!(outline.roots[0].collapsed, "a collapsed");
            assert_eq!(
                flatten_visible(&outline.roots).len(),
                1,
                "collapsed child hidden from the visible order",
            );
            assert!(
                !page.read(cx).dirty(),
                "collapse is view-only state — must not dirty the page",
            );
        });

        cx.simulate_keystrokes(&shortcut);
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(!page.read(cx).outline().roots[0].collapsed, "re-expanded");
        });
    }

    #[gpui::test]
    fn backspace_removes_the_block_and_focuses_previous(cx: &mut TestAppContext) {
        delete_key_removes_the_block_and_focuses_previous(cx, "backspace");
    }

    #[gpui::test]
    fn delete_removes_the_block_and_focuses_previous(cx: &mut TestAppContext) {
        delete_key_removes_the_block_and_focuses_previous(cx, "delete");
    }

    fn delete_key_removes_the_block_and_focuses_previous(cx: &mut TestAppContext, key: &str) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n  - c\n- d\n");
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes(key);
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                page.read(cx).outline().serialize(),
                "- a\n- d\n",
                "the block and its children are gone",
            );
        });
        assert_eq!(focused_id(cx, &ov), Some(a), "previous block takes focus");
    }

    /// The first block hands focus to the block after its subtree, not to
    /// its own (also deleted) child.
    #[gpui::test]
    fn deleting_the_first_block_focuses_the_next_survivor(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - a1\n- b\n");
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });

        focus_block(cx, &ov, a);
        cx.simulate_keystrokes("delete");
        cx.run_until_parked();

        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- b\n"));
        assert_eq!(focused_id(cx, &ov), Some(b));
    }

    #[gpui::test]
    fn deleting_the_only_block_is_noop(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - a1\n");
        let a = cx.read(|cx| page.read(cx).outline().roots[0].id);

        focus_block(cx, &ov, a);
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();

        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- a\n  - a1\n"));
    }

    #[gpui::test]
    fn undo_restores_deleted_blocks_in_reverse_order(cx: &mut TestAppContext) {
        let body = "- a\n- b\n  - c\n- d\n";
        let (page, ov, cx) = mount(cx, body);
        let (c, d) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[1].children[0].id, outline.roots[2].id)
        });
        let undo = format!("{}-z", crate::cmd_key());

        // Deleting d focuses c, the previous visible block; then c goes too.
        focus_block(cx, &ov, d);
        cx.simulate_keystrokes("backspace backspace");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- a\n- b\n"));

        cx.simulate_keystrokes(&undo);
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- a\n- b\n  - c\n"));
        assert_eq!(
            focused_id(cx, &ov),
            Some(c),
            "the restored block is focused"
        );

        cx.simulate_keystrokes(&undo);
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), body));
        assert_eq!(focused_id(cx, &ov), Some(d));

        // Nothing left to undo.
        cx.simulate_keystrokes(&undo);
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), body));
    }

    /// Mid-edit, Tab/Shift-Tab indent and outdent only with the caret at the
    /// very start, and editing carries on in the moved block.
    #[gpui::test]
    fn tab_at_start_of_edit_indents_and_outdents(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[1].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("enter home tab");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- a\n  - b\n"));

        cx.simulate_input("x");
        cx.simulate_keystrokes("left shift-tab");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- a\n- xb\n"));
        let bv = cx.read(|cx| ov.read(cx).block_view(b).expect("b mounted").clone());
        cx.read(|cx| assert!(bv.read(cx).is_editing(), "still editing"));

        // Away from the start, Tab does nothing.
        cx.simulate_keystrokes("right tab");
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(page.read(cx).outline().serialize(), "- a\n- xb\n"));
    }

    /// The `!editing` guard in `BLOCK_CONTEXT`: while a block's `TextInput` is
    /// focused, structural outline keys must not leak into outline ops through
    /// the wrapper's ancestor key context. Plain Up/Down are intentionally
    /// bound by `BlockView` now; their shifted variants remain unbound.
    #[gpui::test]
    fn structural_outline_keys_do_not_fire_while_editing(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[1].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.read(|cx| {
            let bv = ov.read(cx).block_view(b).expect("b mounted").clone();
            assert!(bv.read(cx).is_editing(), "precondition: editing");
        });

        cx.simulate_keystrokes("shift-up shift-down");
        cx.run_until_parked();

        cx.read(|cx| {
            let bv = ov.read(cx).block_view(b).expect("b mounted").clone();
            assert!(
                bv.read(cx).is_editing(),
                "vertical movement mid-edit must not move focus out of the editor",
            );
            let input = bv
                .read(cx)
                .input_entity_for_test()
                .expect("input remains mounted");
            assert_eq!(
                input.read(cx).selected_range(),
                1..1,
                "shifted vertical movement must not disturb the caret",
            );
        });

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 2, "tab mid-edit must not indent");
        });
    }

    #[test]
    fn flatten_respects_collapsed() {
        let mut a = Block::new("a");
        let mut b = Block::new("b");
        let c = Block::new("c");
        b.children.push(c);
        a.children.push(b);

        let ids: Vec<BlockId> = flatten_visible(std::slice::from_ref(&a))
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(ids.len(), 3);

        // Collapse the middle block; its child should disappear.
        a.children[0].collapsed = true;
        let ids: Vec<BlockId> = flatten_visible(std::slice::from_ref(&a))
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(ids.len(), 2);
    }
}
