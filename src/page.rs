use gpui::{AppContext, Context, Entity, EventEmitter, SharedString};

use crate::outline::{Block, BlockId, Outline};
use crate::outline_view::OutlineView;

#[derive(Debug, Clone)]
pub enum PageEvent {
    Saved,
}

pub struct Page {
    name: SharedString,
    outline: Outline,
    /// Serialized form of `outline`. Kept in sync with edits so the registry
    /// save path stays cheap and `body()` can hand out a `&SharedString`.
    body: SharedString,
    view: Entity<OutlineView>,
    dirty: bool,
    /// Outlines to go back to, most recent last. Every edit pushes the
    /// outline as it was before, so undo covers text, structure and deletes
    /// alike.
    undo: Vec<Snapshot>,
    /// Outlines undone since the last edit, most recent last.
    redo: Vec<Snapshot>,
    /// The block whose keystrokes are being folded into the top `undo`
    /// snapshot, so one stretch of typing undoes as a single step. Any other
    /// edit, an undo/redo, or `end_typing_group` ends the stretch.
    typing: Option<BlockId>,
}

/// A whole outline saved for undo/redo, with the block to focus once it is
/// back on screen. It keeps `BlockId`s, so restored blocks reuse their cached
/// `BlockView`s, and fold state: collapse toggles aren't recorded as steps,
/// but a step brings back the folds it was taken with.
struct Snapshot {
    outline: Outline,
    focus: BlockId,
}

impl EventEmitter<PageEvent> for Page {}

impl Page {
    pub fn new(name: SharedString, body: &str, cx: &mut Context<Self>) -> Self {
        let mut outline = Outline::parse(body);
        // A block-based view needs at least one block to be usable. Seeding
        // with an empty block on an empty file does not mark the page dirty:
        // the save path only writes when the user actually edits.
        if outline.roots.is_empty() {
            outline.roots.push(Block::new(""));
        }
        let body: SharedString = outline.serialize().into();
        let page = cx.entity();
        let view = cx.new(|cx| OutlineView::new(page, cx));
        Self {
            name,
            outline,
            body,
            view,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            typing: None,
        }
    }

    #[must_use]
    pub fn name(&self) -> &SharedString {
        &self.name
    }

    #[must_use]
    pub fn body(&self) -> &SharedString {
        &self.body
    }

    #[must_use]
    pub fn outline(&self) -> &Outline {
        &self.outline
    }

    #[must_use]
    pub fn view(&self) -> &Entity<OutlineView> {
        &self.view
    }

    #[must_use]
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn set_block_text(&mut self, id: BlockId, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        // Flushing identical text (e.g. a focus/blur cycle with no typing)
        // must not dirty the page or rewrite the file (#39).
        if self.outline.get(id) == Some(text.as_str()) {
            return;
        }
        let before = (self.typing != Some(id)).then(|| self.outline.clone());
        if !self.outline.set_text(id, text) {
            return;
        }
        if let Some(before) = before {
            self.push_undo(before, id);
            self.typing = Some(id);
        }
        self.body = self.outline.serialize().into();
        self.dirty = true;
        cx.notify();
    }

    /// Insert a sibling block immediately after `id`. Returns the new
    /// block's id, or `None` if `id` is not in the outline.
    pub fn insert_block_after(
        &mut self,
        id: BlockId,
        text: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Option<BlockId> {
        let mut new_id = None;
        self.apply_outline_edit(id, cx, |o| {
            new_id = o.insert_after(id, text);
            new_id.is_some()
        });
        new_id
    }

    /// Insert a sibling block immediately before `id`. Returns the new
    /// block's id, or `None` if `id` is not in the outline.
    pub fn insert_block_before(
        &mut self,
        id: BlockId,
        text: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Option<BlockId> {
        let mut new_id = None;
        self.apply_outline_edit(id, cx, |o| {
            new_id = o.insert_before(id, text);
            new_id.is_some()
        });
        new_id
    }

    /// Sets `id`'s text to `head` and inserts `tail` as a new sibling right
    /// after it, as one undo step. Returns the new block's id.
    pub fn split_block(
        &mut self,
        id: BlockId,
        head: String,
        tail: String,
        cx: &mut Context<Self>,
    ) -> Option<BlockId> {
        let mut new_id = None;
        self.apply_outline_edit(id, cx, |o| {
            new_id = o.insert_after(id, tail);
            new_id.is_some() && o.set_text(id, head)
        });
        new_id
    }

    /// Applies a structural outline edit. If `op` reports a change, the
    /// outline as it was is pushed for undo (focusing `focus` when it comes
    /// back), the body is reserialized, the page marked dirty, and observers
    /// notified. Returns whether anything changed.
    fn apply_outline_edit(
        &mut self,
        focus: BlockId,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut Outline) -> bool,
    ) -> bool {
        let before = self.outline.clone();
        if !op(&mut self.outline) {
            return false;
        }
        self.push_undo(before, focus);
        self.typing = None;
        self.body = self.outline.serialize().into();
        self.dirty = true;
        cx.notify();
        true
    }

    fn push_undo(&mut self, outline: Outline, focus: BlockId) {
        self.undo.push(Snapshot { outline, focus });
        self.redo.clear();
    }

    /// Ends the current stretch of typing, so the next keystroke starts a
    /// new undo step. Called whenever a block enters editing.
    pub fn end_typing_group(&mut self) {
        self.typing = None;
    }

    pub fn indent_block(&mut self, id: BlockId, cx: &mut Context<Self>) -> bool {
        self.apply_outline_edit(id, cx, |o| o.indent(id))
    }

    pub fn outdent_block(&mut self, id: BlockId, cx: &mut Context<Self>) -> bool {
        self.apply_outline_edit(id, cx, |o| o.outdent(id))
    }

    /// Indents each of `ids` as one undo step; see `Outline::indent_all`.
    pub fn indent_blocks(&mut self, ids: &[BlockId], cx: &mut Context<Self>) -> bool {
        let Some(&first) = ids.first() else {
            return false;
        };
        self.apply_outline_edit(first, cx, |o| o.indent_all(ids))
    }

    /// Outdents each of `ids` as one undo step; see `Outline::outdent_all`.
    pub fn outdent_blocks(&mut self, ids: &[BlockId], cx: &mut Context<Self>) -> bool {
        let Some(&first) = ids.first() else {
            return false;
        };
        self.apply_outline_edit(first, cx, |o| o.outdent_all(ids))
    }

    pub fn move_block_up(&mut self, id: BlockId, cx: &mut Context<Self>) -> bool {
        self.apply_outline_edit(id, cx, |o| o.move_up(id))
    }

    pub fn move_block_down(&mut self, id: BlockId, cx: &mut Context<Self>) -> bool {
        self.apply_outline_edit(id, cx, |o| o.move_down(id))
    }

    /// Called when an edit of `id` ends; see `Outline::split_bullet_lines`.
    pub fn split_bullet_lines(&mut self, id: BlockId, cx: &mut Context<Self>) -> bool {
        self.apply_outline_edit(id, cx, |o| o.split_bullet_lines(id))
    }

    /// See `Outline::paste_blocks`.
    pub fn paste_blocks(
        &mut self,
        id: BlockId,
        pasted: Vec<Block>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.apply_outline_edit(id, cx, |o| o.paste_blocks(id, pasted))
    }

    /// See `Outline::merge`. Undo refocuses `id`.
    pub fn merge_block(
        &mut self,
        id: BlockId,
        into: BlockId,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let mut join = None;
        self.apply_outline_edit(id, cx, |o| {
            join = o.merge(id, into);
            join.is_some()
        });
        join
    }

    /// Deletes the block (and its subtree) from the outline.
    pub fn delete_block(&mut self, id: BlockId, cx: &mut Context<Self>) -> bool {
        self.apply_outline_edit(id, cx, |o| o.delete(id).is_some())
    }

    /// Deletes each of `ids` with its subtree, as one undo step that
    /// refocuses the first.
    pub fn delete_blocks(&mut self, ids: &[BlockId], cx: &mut Context<Self>) -> bool {
        let Some(&first) = ids.first() else {
            return false;
        };
        self.apply_outline_edit(first, cx, |o| {
            // Not `any`, which would stop after the first deletion.
            let deleted = ids.iter().filter(|&&id| o.delete(id).is_some()).count();
            deleted > 0
        })
    }

    /// Steps back to the outline before the most recent edit. Returns the
    /// block that edit touched, for the view to focus, or `None` when there
    /// is nothing to undo.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> Option<BlockId> {
        let snapshot = self.undo.pop()?;
        let focus = snapshot.focus;
        let current = std::mem::replace(&mut self.outline, snapshot.outline);
        self.redo.push(Snapshot {
            outline: current,
            focus,
        });
        self.history_stepped(cx);
        Some(focus)
    }

    /// Re-applies the most recently undone edit; see `undo`.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> Option<BlockId> {
        let snapshot = self.redo.pop()?;
        let focus = snapshot.focus;
        let current = std::mem::replace(&mut self.outline, snapshot.outline);
        self.undo.push(Snapshot {
            outline: current,
            focus,
        });
        self.history_stepped(cx);
        Some(focus)
    }

    fn history_stepped(&mut self, cx: &mut Context<Self>) {
        self.typing = None;
        self.body = self.outline.serialize().into();
        self.dirty = true;
        cx.notify();
    }

    /// Collapse state is view-only — it is not serialized into the body —
    /// so toggling must re-render but must not dirty the page.
    pub fn toggle_collapse(&mut self, id: BlockId, cx: &mut Context<Self>) -> bool {
        if self.outline.toggle_collapse(id) {
            cx.notify();
            true
        } else {
            false
        }
    }

    pub fn mark_saved(&mut self, cx: &mut Context<Self>) {
        if self.dirty {
            self.dirty = false;
            cx.notify();
        }
        cx.emit(PageEvent::Saved);
    }

    /// Test helper that replaces the outline with a single root block whose
    /// text is `text`. Marks the page dirty so save paths fire.
    #[cfg(test)]
    pub fn set_body_for_test(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let mut outline = Outline::default();
        outline.roots.push(Block::new(text));
        let serialized: SharedString = outline.serialize().into();
        if serialized == self.body {
            return;
        }
        self.outline = outline;
        self.body = serialized;
        self.dirty = true;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn empty_body_seeds_a_single_block(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "", cx));
        cx.read(|cx| {
            let p = page.read(cx);
            assert_eq!(p.outline().roots.len(), 1);
            assert_eq!(p.outline().roots[0].text, "");
            assert!(!p.dirty(), "seeding must not mark the page dirty");
        });
    }

    #[gpui::test]
    fn preloaded_body_parses_into_outline(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "- hi\n- there\n", cx));
        cx.read(|cx| {
            let p = page.read(cx);
            assert_eq!(p.outline().roots.len(), 2);
            assert_eq!(p.body().as_ref(), "- hi\n- there\n");
            assert!(!p.dirty());
        });
    }

    #[gpui::test]
    fn set_block_text_marks_dirty_and_updates_body(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "- hi\n", cx));
        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);

        cx.update(|cx| {
            page.update(cx, |p, cx| p.set_block_text(first_id, "hello", cx));
        });

        cx.read(|cx| {
            let p = page.read(cx);
            assert_eq!(p.outline().get(first_id), Some("hello"));
            assert_eq!(p.body().as_ref(), "- hello\n");
            assert!(p.dirty());
        });
    }

    /// Regression test for #39: flushing text identical to the block's
    /// current text (e.g. a focus/blur cycle with no typing) must not mark
    /// the page dirty.
    #[gpui::test]
    fn set_block_text_with_unchanged_text_stays_clean(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "- hi\n", cx));
        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);

        cx.update(|cx| {
            page.update(cx, |p, cx| p.set_block_text(first_id, "hi", cx));
        });

        cx.read(|cx| {
            assert!(
                !page.read(cx).dirty(),
                "identical text must not dirty the page",
            );
        });
    }

    /// clean → dirty via `insert_block_after`. Companion to the
    /// `set_block_text` transition test above.
    #[gpui::test]
    fn transition_clean_to_dirty_via_insert_block_after(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "- hi\n", cx));
        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);
        assert!(!cx.read(|cx| page.read(cx).dirty()));

        let new_id = cx
            .update(|cx| page.update(cx, |p, cx| p.insert_block_after(first_id, "new", cx)))
            .expect("insert returned id");

        cx.read(|cx| {
            let p = page.read(cx);
            assert!(p.dirty(), "insert_block_after marks the page dirty");
            assert_eq!(p.outline().get(new_id), Some("new"));
        });
    }

    /// dirty → clean via `mark_saved`. The registry calls this after a
    /// successful write; the test exercises the page in isolation.
    #[gpui::test]
    fn transition_dirty_to_clean_via_mark_saved(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "- hi\n", cx));
        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);

        cx.update(|cx| {
            page.update(cx, |p, cx| p.set_block_text(first_id, "edited", cx));
        });
        assert!(cx.read(|cx| page.read(cx).dirty()), "precondition: dirty");

        cx.update(|cx| {
            page.update(cx, Page::mark_saved);
        });

        assert!(!cx.read(|cx| page.read(cx).dirty()));
    }
}
