//! Outline-level container that composes `BlockView` children with indent +
//! bullet glyphs. See issue #6.
//!
//! `BlockView` entities are cached by `BlockId` so focus state and any
//! mounted `TextInput` survive re-renders. Ids missing from the page's
//! outline (deleted blocks) are pruned at render time.

use std::collections::{HashMap, HashSet};

use gpui::{
    div, px, AppContext, Context, Entity, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window,
};

use crate::block_render::theme;
use crate::block_view::BlockView;
use crate::outline::{Block, BlockId};
use crate::page::Page;

pub struct OutlineView {
    page: Entity<Page>,
    blocks: HashMap<BlockId, Entity<BlockView>>,
    _page_sub: Subscription,
}

impl OutlineView {
    pub fn new(page: Entity<Page>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&page, |_, _, cx| cx.notify());
        Self {
            page,
            blocks: HashMap::new(),
            _page_sub: sub,
        }
    }

    /// Focus the first root block in the outline, creating its `BlockView` if
    /// needed. No-op for an empty outline.
    pub fn focus_first_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first_id) = self.page.read(cx).outline().first_block_id() else {
            return;
        };
        let bv = self.get_or_create(first_id, window, cx);
        let handle = bv.focus_handle(cx);
        window.focus(&handle, cx);
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
        self.blocks.insert(id, bv.clone());
        bv
    }

    #[cfg(test)]
    #[must_use]
    pub fn block_view(&self, id: BlockId) -> Option<&Entity<BlockView>> {
        self.blocks.get(&id)
    }
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

fn collect_all_ids(blocks: &[Block], out: &mut HashSet<BlockId>) {
    for b in blocks {
        out.insert(b.id);
        collect_all_ids(&b.children, out);
    }
}

impl Render for OutlineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (flat, all_ids) = {
            let outline = self.page.read(cx).outline();
            let mut all = HashSet::new();
            collect_all_ids(&outline.roots, &mut all);
            (flatten_visible(&outline.roots), all)
        };
        self.blocks.retain(|id, _| all_ids.contains(id));

        let mut root = div().flex().flex_col().gap_1();
        for (depth, id) in flat {
            let bv = self.get_or_create(id, window, cx);
            #[allow(clippy::cast_precision_loss)]
            let indent = px(16.0) * depth as f32;
            let row = div()
                .flex()
                .flex_row()
                .items_start()
                .pl(indent)
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .text_color(theme::fg_muted())
                        .child(SharedString::from("•")),
                )
                .child(bv);
            root = root.child(row);
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::Block;

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
