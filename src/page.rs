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
}

impl EventEmitter<PageEvent> for Page {}

impl Page {
    pub fn new(name: SharedString, body: String, cx: &mut Context<Self>) -> Self {
        let mut outline = Outline::parse(&body);
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
        if !self.outline.set_text(id, text) {
            return;
        }
        self.body = self.outline.serialize().into();
        self.dirty = true;
        cx.notify();
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
        let page = cx.new(|cx| Page::new("foo".into(), String::new(), cx));
        cx.read(|cx| {
            let p = page.read(cx);
            assert_eq!(p.outline().roots.len(), 1);
            assert_eq!(p.outline().roots[0].text, "");
            assert!(!p.dirty(), "seeding must not mark the page dirty");
        });
    }

    #[gpui::test]
    fn preloaded_body_parses_into_outline(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "- hi\n- there\n".into(), cx));
        cx.read(|cx| {
            let p = page.read(cx);
            assert_eq!(p.outline().roots.len(), 2);
            assert_eq!(p.body().as_ref(), "- hi\n- there\n");
            assert!(!p.dirty());
        });
    }

    #[gpui::test]
    fn set_block_text_marks_dirty_and_updates_body(cx: &mut TestAppContext) {
        let page = cx.new(|cx| Page::new("foo".into(), "- hi\n".into(), cx));
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
}
