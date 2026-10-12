use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlockId(u64);

static NEXT_BLOCK_ID: AtomicU64 = AtomicU64::new(1);

impl BlockId {
    fn fresh() -> Self {
        Self(NEXT_BLOCK_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub id: BlockId,
    pub text: String,
    pub children: Vec<Block>,
    pub collapsed: bool,
}

impl Block {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            id: BlockId::fresh(),
            text: text.into(),
            children: Vec::new(),
            collapsed: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Outline {
    /// Lines before the first bullet (Logseq page properties such as
    /// `title::`), kept verbatim so a save writes them back.
    pub preamble: String,
    pub roots: Vec<Block>,
}

/// Width (spaces = 1, tab = 2) and byte length of a line's leading whitespace.
fn leading_indent(line: &str) -> (usize, usize) {
    let mut width = 0;
    let mut bytes = 0;
    for b in line.bytes() {
        match b {
            b' ' => width += 1,
            b'\t' => width += 2,
            _ => break,
        }
        bytes += 1;
    }
    (width, bytes)
}

/// Returns `(marker indent, text column, text)` for a bullet line. The text
/// column is where continuation lines of this block must be indented to.
fn parse_bullet_line(line: &str) -> Option<(usize, usize, String)> {
    let (indent, bytes_consumed) = leading_indent(line);
    let rest = &line[bytes_consumed..];
    let mut it = rest.chars();
    let marker = it.next()?;
    if marker != '-' && marker != '*' {
        return None;
    }
    let Some(after) = it.next() else {
        // A bare marker is an empty block; Logseq writes these as `-`.
        return Some((indent, indent + 2, String::new()));
    };
    if after != ' ' && after != '\t' {
        return None;
    }
    let text_col = indent + 1 + if after == '\t' { 2 } else { 1 };
    let text_start = bytes_consumed + marker.len_utf8() + after.len_utf8();
    Some((indent, text_col, line[text_start..].to_string()))
}

fn is_fence(text: &str) -> bool {
    text.trim_start().starts_with("```")
}

/// `line` with up to `cols` columns of leading whitespace removed.
fn strip_indent(line: &str, cols: usize) -> &str {
    let mut consumed = 0;
    let mut bytes = 0;
    for b in line.bytes() {
        if consumed >= cols {
            break;
        }
        match b {
            b' ' => consumed += 1,
            b'\t' => consumed += 2,
            _ => break,
        }
        bytes += 1;
    }
    &line[bytes..]
}

/// Appends `(indent, text_col, text)` from `parse_bullet_line` under the
/// right parent and returns the new block's `(text_col, in_fence)`.
fn push_bullet(
    roots: &mut Vec<Block>,
    path: &mut Vec<(usize, usize)>,
    (indent, text_col, text): (usize, usize, String),
) -> (usize, bool) {
    while let Some(&(top_indent, _)) = path.last() {
        if top_indent >= indent {
            path.pop();
        } else {
            break;
        }
    }
    let in_fence = is_fence(&text);
    let parent_indices: Vec<usize> = path.iter().map(|(_, i)| *i).collect();
    let parent = children_mut(roots, &parent_indices);
    let idx = parent.len();
    parent.push(Block::new(text));
    path.push((indent, idx));
    (text_col, in_fence)
}

/// The block most recently pushed by `parse`, addressed by its parse `path`.
fn last_block_mut<'a>(roots: &'a mut Vec<Block>, path: &[(usize, usize)]) -> Option<&'a mut Block> {
    let (&(_, last), parents) = path.split_last()?;
    let parent_indices: Vec<usize> = parents.iter().map(|(_, i)| *i).collect();
    Some(&mut children_mut(roots, &parent_indices)[last])
}

fn children_mut<'a>(roots: &'a mut Vec<Block>, path: &[usize]) -> &'a mut Vec<Block> {
    let mut v: &mut Vec<Block> = roots;
    for &i in path {
        v = &mut v[i].children;
    }
    v
}

impl Outline {
    /// Every line of `input` lands somewhere — a save rewrites the whole
    /// file from the outline, so a dropped line is deleted from disk. Lines
    /// before the first bullet are the preamble; after it, any line that does
    /// not start a new block continues the most recent one.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let mut preamble = String::new();
        let mut roots: Vec<Block> = Vec::new();
        // Stack of (indent_width, child_index_in_parent) describing the path from the
        // root to the most recent block. `path.len()` is the nesting level.
        let mut path: Vec<(usize, usize)> = Vec::new();

        // The block currently accepting continuation lines: its text column,
        // and whether a fence it opened is still unclosed.
        let mut open: Option<(usize, bool)> = None;
        // Blank lines are held back until the block continues, so a blank
        // between two paragraphs of a block is kept while one between blocks
        // is just spacing.
        let mut pending_blanks = 0;

        for line in input.lines() {
            let bullet = parse_bullet_line(line);
            let Some((text_col, in_fence)) = open else {
                if let Some(bullet) = bullet {
                    open = Some(push_bullet(&mut roots, &mut path, bullet));
                } else {
                    preamble.push_str(line);
                    preamble.push('\n');
                }
                continue;
            };

            // Inside a fence, a line at the block's depth is code even if it
            // looks like a bullet; a shallower bullet still ends the block.
            let in_code = in_fence && leading_indent(line).0 >= text_col;
            match bullet.filter(|_| !in_code) {
                Some(bullet) => {
                    pending_blanks = 0;
                    open = Some(push_bullet(&mut roots, &mut path, bullet));
                }
                None if line.trim().is_empty() => pending_blanks += 1,
                None => {
                    let rest = strip_indent(line, text_col);
                    if let Some(block) = last_block_mut(&mut roots, &path) {
                        for _ in 0..=pending_blanks {
                            block.text.push('\n');
                        }
                        block.text.push_str(rest);
                    }
                    pending_blanks = 0;
                    if is_fence(rest) {
                        open = Some((text_col, !in_fence));
                    }
                }
            }
        }

        Outline { preamble, roots }
    }

    #[must_use]
    pub fn serialize(&self) -> String {
        fn walk(out: &mut String, blocks: &[Block], depth: usize) {
            // Continuation lines are re-indented to the bullet's text column,
            // which for `"  " * depth + "- "` is `depth * 2 + 2`.
            let text_col = "  ".repeat(depth + 1);
            for b in blocks {
                let mut lines = b.text.split('\n');
                out.push_str(&text_col[2..]);
                // An empty first line is a bare `-`, as Logseq writes it.
                let first = lines.next().unwrap_or_default();
                if first.is_empty() {
                    out.push('-');
                } else {
                    out.push_str("- ");
                    out.push_str(first);
                }
                out.push('\n');
                for line in lines {
                    if !line.is_empty() {
                        out.push_str(&text_col);
                        out.push_str(line);
                    }
                    out.push('\n');
                }
                walk(out, &b.children, depth + 1);
            }
        }
        let mut out = self.preamble.clone();
        walk(&mut out, &self.roots, 0);
        out
    }

    #[must_use]
    pub fn path_to(&self, id: BlockId) -> Option<Vec<usize>> {
        fn find(blocks: &[Block], id: BlockId, path: &mut Vec<usize>) -> bool {
            for (i, b) in blocks.iter().enumerate() {
                path.push(i);
                if b.id == id {
                    return true;
                }
                if find(&b.children, id, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
        let mut path = Vec::new();
        find(&self.roots, id, &mut path).then_some(path)
    }

    fn take(&mut self, path: &[usize]) -> Block {
        let (last, parent_path) = path.split_last().expect("non-empty path");
        let siblings = children_mut(&mut self.roots, parent_path);
        siblings.remove(*last)
    }

    pub fn indent(&mut self, id: BlockId) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let Some((&last, parent_path)) = path.split_last() else {
            return false;
        };
        if last == 0 {
            return false;
        }
        let block = self.take(&path);
        let prev_sibling = &mut children_mut(&mut self.roots, parent_path)[last - 1];
        prev_sibling.children.push(block);
        true
    }

    pub fn outdent(&mut self, id: BlockId) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        if path.len() < 2 {
            return false;
        }
        let block = self.take(&path);
        let Some((&parent_idx, grandparent_path)) = path[..path.len() - 1].split_last() else {
            return false;
        };
        let grandparent = children_mut(&mut self.roots, grandparent_path);
        grandparent.insert(parent_idx + 1, block);
        true
    }

    pub fn move_up(&mut self, id: BlockId) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let Some((&last, parent_path)) = path.split_last() else {
            return false;
        };
        if last == 0 {
            return false;
        }
        let siblings = children_mut(&mut self.roots, parent_path);
        siblings.swap(last - 1, last);
        true
    }

    pub fn move_down(&mut self, id: BlockId) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let Some((&last, parent_path)) = path.split_last() else {
            return false;
        };
        let siblings = children_mut(&mut self.roots, parent_path);
        if last + 1 >= siblings.len() {
            return false;
        }
        siblings.swap(last, last + 1);
        true
    }

    pub fn toggle_collapse(&mut self, id: BlockId) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let Some((&last, parent_path)) = path.split_last() else {
            return false;
        };
        let siblings = children_mut(&mut self.roots, parent_path);
        siblings[last].collapsed = !siblings[last].collapsed;
        true
    }

    pub fn insert_after(&mut self, id: BlockId, text: impl Into<String>) -> Option<BlockId> {
        let path = self.path_to(id)?;
        let (&last, parent_path) = path.split_last()?;
        let block = Block::new(text);
        let new_id = block.id;
        let siblings = children_mut(&mut self.roots, parent_path);
        siblings.insert(last + 1, block);
        Some(new_id)
    }

    pub fn delete(&mut self, id: BlockId) -> Option<Block> {
        let path = self.path_to(id)?;
        Some(self.take(&path))
    }

    #[must_use]
    pub fn get(&self, id: BlockId) -> Option<&str> {
        fn find(blocks: &[Block], id: BlockId) -> Option<&str> {
            for b in blocks {
                if b.id == id {
                    return Some(b.text.as_str());
                }
                if let Some(t) = find(&b.children, id) {
                    return Some(t);
                }
            }
            None
        }
        find(&self.roots, id)
    }

    pub fn set_text(&mut self, id: BlockId, text: impl Into<String>) -> bool {
        fn walk(blocks: &mut [Block], id: BlockId, text: &mut Option<String>) -> bool {
            for b in blocks {
                if b.id == id {
                    b.text = text.take().expect("set_text applied once");
                    return true;
                }
                if walk(&mut b.children, id, text) {
                    return true;
                }
            }
            false
        }
        let mut text = Some(text.into());
        walk(&mut self.roots, id, &mut text)
    }

    #[must_use]
    pub fn first_block_id(&self) -> Option<BlockId> {
        self.roots.first().map(|b| b.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn ids(outline: &Outline) -> Vec<BlockId> {
        fn walk(out: &mut Vec<BlockId>, blocks: &[Block]) {
            for b in blocks {
                out.push(b.id);
                walk(out, &b.children);
            }
        }
        let mut v = Vec::new();
        walk(&mut v, &outline.roots);
        v
    }

    fn by_text(outline: &Outline, text: &str) -> BlockId {
        fn find(blocks: &[Block], text: &str) -> Option<BlockId> {
            for b in blocks {
                if b.text == text {
                    return Some(b.id);
                }
                if let Some(id) = find(&b.children, text) {
                    return Some(id);
                }
            }
            None
        }
        find(&outline.roots, text).unwrap_or_else(|| panic!("no block with text {text:?}"))
    }

    #[test]
    fn parses_flat_list() {
        let o = Outline::parse("- a\n- b\n- c\n");
        assert_eq!(o.roots.len(), 3);
        assert_eq!(o.roots[0].text, "a");
        assert!(o.roots[0].children.is_empty());
    }

    #[test]
    fn parses_nested_three_levels() {
        let src = "- a\n  - b\n    - c\n- d\n";
        let o = Outline::parse(src);
        assert_eq!(o.roots.len(), 2);
        assert_eq!(o.roots[0].text, "a");
        assert_eq!(o.roots[0].children.len(), 1);
        assert_eq!(o.roots[0].children[0].text, "b");
        assert_eq!(o.roots[0].children[0].children[0].text, "c");
        assert_eq!(o.roots[1].text, "d");
    }

    #[test]
    fn accepts_star_marker() {
        let o = Outline::parse("* a\n  * b\n");
        assert_eq!(o.roots[0].text, "a");
        assert_eq!(o.roots[0].children[0].text, "b");
    }

    #[test]
    fn tab_indent_counts_as_two_spaces() {
        let o = Outline::parse("- a\n\t- b\n");
        assert_eq!(o.roots[0].children[0].text, "b");
    }

    #[test]
    fn non_bullet_lines_are_preserved() {
        let o = Outline::parse("preamble\n- a\nnot a bullet\n- b\n");
        assert_eq!(o.roots.len(), 2);
        assert_eq!(o.roots[0].text, "a\nnot a bullet");
        assert_eq!(o.roots[1].text, "b");
        assert_eq!(o.serialize(), "preamble\n- a\n  not a bullet\n- b\n");
    }

    #[test]
    fn bare_dash_is_an_empty_block_that_keeps_its_children() {
        let o = Outline::parse("- a\n-\n\t- kid\n- b\n");
        assert_eq!(o.roots.len(), 3);
        assert_eq!(o.roots[1].text, "");
        assert_eq!(o.roots[1].children.len(), 1);
        assert_eq!(o.roots[1].children[0].text, "kid");
        assert!(o.roots[0].children.is_empty());
    }

    #[test]
    fn page_properties_before_the_first_bullet_roundtrip() {
        let src = "title:: Foo\nalias:: bar\n\n- a\n";
        assert_eq!(Outline::parse(src).serialize(), src);
    }

    #[test]
    fn shallow_fence_close_still_closes_the_fence() {
        let o = Outline::parse("- ```py\n  def f():\n ```\n- next\n");
        assert_eq!(o.roots.len(), 2);
        assert_eq!(o.roots[0].text, "```py\ndef f():\n```");
        assert_eq!(o.roots[1].text, "next");
    }

    #[test]
    fn unclosed_fence_ends_at_a_shallower_bullet() {
        let o = Outline::parse("- ```\n  code\n- next\n");
        assert_eq!(o.roots.len(), 2);
        assert_eq!(o.roots[0].text, "```\ncode");
    }

    #[test]
    fn blank_line_between_paragraphs_of_a_block_is_kept() {
        let o = Outline::parse("- one\n\n  two\n- b\n");
        assert_eq!(o.roots[0].text, "one\n\ntwo");
    }

    #[test]
    fn blank_lines_between_blocks_are_spacing() {
        let o = Outline::parse("- a\n\n- b\n\n");
        assert_eq!(o.roots[0].text, "a");
        assert_eq!(o.roots[1].text, "b");
        assert_eq!(o.serialize(), "- a\n- b\n");
    }

    #[test]
    fn shallow_continuation_line_joins_the_block() {
        let o = Outline::parse("  - a\n b\n");
        assert_eq!(o.roots[0].text, "a\nb");
    }

    fn content_lines(src: &str) -> Vec<String> {
        src.lines()
            .map(|l| l.trim().trim_start_matches(['-', '*']).trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Every edit rewrites the whole file from the parsed outline, so any
    /// line `parse` drops is deleted from disk on the next keystroke.
    #[rstest]
    #[case::bare_dash_with_children("- a\n-\n\t- kid\n- b\n")]
    #[case::preamble("title:: Foo\ntags:: x, y\n\n- a\n  - b\n")]
    #[case::only_prose("just a paragraph\nand another\n")]
    #[case::shallow_fence("- ```py\n  def f():\n      return 1\n ```\n- next\n")]
    #[case::shallow_prose("- a\n b\nc\n- d\n")]
    #[case::properties("- parent\n  collapsed:: true\n  - kid\n    id:: 64a0\n")]
    #[case::star_and_tabs("*\ta\n\t*\tb\n\t\t-\n")]
    fn parse_serialize_preserves_content(#[case] src: &str) {
        let out = Outline::parse(src).serialize();
        assert_eq!(
            content_lines(&out),
            content_lines(src),
            "lost content: {out:?}"
        );
        assert_eq!(Outline::parse(&out).serialize(), out, "not idempotent");
    }

    #[test]
    fn roundtrip_canonical_forms() {
        for src in [
            "- a\n",
            "- a\n- b\n",
            "- a\n  - b\n    - c\n- d\n",
            "-\n",
            "- a\n-\n  - kid\n",
        ] {
            let o = Outline::parse(src);
            assert_eq!(o.serialize(), src, "roundtrip failed for {src:?}");
        }
    }

    #[rstest]
    #[case::tabs("*\ta\n\t- b\n")]
    #[case::fence("- ```calc\n  1 + 1\n  ```\n")]
    #[case::nested_fence("- Trip\n  - ```calc\n    flights = 420\n    total\n    ```\n")]
    #[case::blank_line_in_fence("- ```calc\n  1 + 1\n\n  2 + 2\n  ```\n")]
    #[case::bullet_in_fence("- ```\n  - not a child\n  ```\n- next\n")]
    #[case::tabs_in_fence("- ```calc\n\t1 + 1\n\t```\n")]
    #[case::fence_then_child("- ```calc\n  1 + 1\n  ```\n  - child\n")]
    fn serialize_is_idempotent_under_reparse(#[case] src: &str) {
        let once = Outline::parse(src).serialize();
        let twice = Outline::parse(&once).serialize();
        assert_eq!(once, twice, "not idempotent for {src:?}");
    }

    #[test]
    fn fence_body_joins_the_block_text() {
        let o = Outline::parse("- Trip\n  - ```calc\n    flights = 420\n    total\n    ```\n");
        let calc = &o.roots[0].children[0];
        assert_eq!(calc.text, "```calc\nflights = 420\ntotal\n```");
        assert!(calc.children.is_empty());
    }

    #[test]
    fn bullet_inside_a_fence_is_not_a_child() {
        let o = Outline::parse("- ```\n  - not a child\n  ```\n- next\n");
        assert_eq!(o.roots.len(), 2);
        assert_eq!(o.roots[0].text, "```\n- not a child\n```");
        assert_eq!(o.roots[1].text, "next");
    }

    #[test]
    fn indented_bullet_after_a_closed_fence_is_a_child() {
        let o = Outline::parse("- ```calc\n  1 + 1\n  ```\n  - child\n");
        assert_eq!(o.roots[0].text, "```calc\n1 + 1\n```");
        assert_eq!(o.roots[0].children.len(), 1);
        assert_eq!(o.roots[0].children[0].text, "child");
    }

    #[test]
    fn path_to_unknown_id_returns_none() {
        let o = Outline::parse("- a\n");
        let ghost = BlockId::fresh();
        assert!(o.path_to(ghost).is_none());
    }

    #[test]
    fn indent_under_previous_sibling() {
        let mut o = Outline::parse("- a\n- b\n");
        let b = by_text(&o, "b");
        assert!(o.indent(b));
        assert_eq!(o.serialize(), "- a\n  - b\n");
    }

    #[test]
    fn indent_first_sibling_is_noop() {
        let mut o = Outline::parse("- a\n- b\n");
        let a = by_text(&o, "a");
        assert!(!o.indent(a));
        assert_eq!(o.serialize(), "- a\n- b\n");
    }

    #[test]
    fn outdent_raises_one_level() {
        let mut o = Outline::parse("- a\n  - b\n");
        let b = by_text(&o, "b");
        assert!(o.outdent(b));
        assert_eq!(o.serialize(), "- a\n- b\n");
    }

    #[test]
    fn outdent_at_root_is_noop() {
        let mut o = Outline::parse("- a\n");
        let a = by_text(&o, "a");
        assert!(!o.outdent(a));
        assert_eq!(o.serialize(), "- a\n");
    }

    #[test]
    fn move_up_and_down_swap_siblings() {
        let mut o = Outline::parse("- a\n- b\n- c\n");
        let b = by_text(&o, "b");
        assert!(o.move_up(b));
        assert_eq!(o.serialize(), "- b\n- a\n- c\n");
        assert!(o.move_down(b));
        assert_eq!(o.serialize(), "- a\n- b\n- c\n");
    }

    #[test]
    fn move_up_at_top_is_noop() {
        let mut o = Outline::parse("- a\n- b\n");
        let a = by_text(&o, "a");
        assert!(!o.move_up(a));
    }

    #[test]
    fn move_down_at_bottom_is_noop() {
        let mut o = Outline::parse("- a\n- b\n");
        let b = by_text(&o, "b");
        assert!(!o.move_down(b));
    }

    #[test]
    fn toggle_collapse_flips_flag() {
        let mut o = Outline::parse("- a\n");
        let a = by_text(&o, "a");
        assert!(!o.roots[0].collapsed);
        assert!(o.toggle_collapse(a));
        assert!(o.roots[0].collapsed);
        assert!(o.toggle_collapse(a));
        assert!(!o.roots[0].collapsed);
    }

    #[test]
    fn insert_after_creates_sibling() {
        let mut o = Outline::parse("- a\n- c\n");
        let a = by_text(&o, "a");
        let new_id = o.insert_after(a, "b").unwrap();
        assert_eq!(o.serialize(), "- a\n- b\n- c\n");
        assert_eq!(o.path_to(new_id), Some(vec![1]));
    }

    #[test]
    fn delete_removes_subtree_and_returns_it() {
        let mut o = Outline::parse("- a\n  - b\n- c\n");
        let a = by_text(&o, "a");
        let removed = o.delete(a).unwrap();
        assert_eq!(removed.text, "a");
        assert_eq!(removed.children.len(), 1);
        assert_eq!(o.serialize(), "- c\n");
    }

    #[test]
    fn get_returns_text_for_nested_block() {
        let o = Outline::parse("- a\n  - b\n");
        let b = by_text(&o, "b");
        assert_eq!(o.get(b), Some("b"));
    }

    #[test]
    fn get_unknown_id_is_none() {
        let o = Outline::parse("- a\n");
        assert!(o.get(BlockId::fresh()).is_none());
    }

    #[test]
    fn set_text_updates_block_and_roundtrips() {
        let mut o = Outline::parse("- a\n  - b\n");
        let b = by_text(&o, "b");
        assert!(o.set_text(b, "BEE"));
        assert_eq!(o.get(b), Some("BEE"));
        assert_eq!(o.serialize(), "- a\n  - BEE\n");
    }

    #[test]
    fn set_text_unknown_id_is_noop() {
        let mut o = Outline::parse("- a\n");
        assert!(!o.set_text(BlockId::fresh(), "x"));
        assert_eq!(o.serialize(), "- a\n");
    }

    #[test]
    fn first_block_id_matches_first_root() {
        let o = Outline::parse("- a\n- b\n");
        assert_eq!(o.first_block_id(), Some(o.roots[0].id));
        let empty = Outline::default();
        assert!(empty.first_block_id().is_none());
    }

    #[test]
    fn block_ids_are_unique_within_outline() {
        let o = Outline::parse("- a\n  - b\n  - c\n- d\n");
        let all = ids(&o);
        let mut dedup = all.clone();
        dedup.sort_by_key(|b| b.0);
        dedup.dedup();
        assert_eq!(all.len(), dedup.len());
    }
}
