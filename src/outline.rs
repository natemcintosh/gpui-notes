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

/// Whether `text` ends inside a code fence: an odd number of its lines are
/// fence lines, by the same rule the parser uses.
#[must_use]
pub fn has_open_fence(text: &str) -> bool {
    text.lines().filter(|line| is_fence(line)).count() % 2 == 1
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

/// A Logseq property line, `key:: value` (the value may be empty).
fn is_property(line: &str) -> bool {
    line.trim_start()
        .split_once("::")
        .is_some_and(|(key, value)| {
            !key.is_empty()
                && !key.contains(char::is_whitespace)
                && (value.is_empty() || value.starts_with(char::is_whitespace))
        })
}

/// Splits the lines before the first bullet into the leading page properties
/// (and blank lines), kept verbatim, and any text after them, which becomes
/// the page's first block so it can be seen and edited.
fn split_preamble(mut preamble: String) -> (String, Option<String>) {
    let mut props_end = 0;
    for line in preamble.split_inclusive('\n') {
        if !(line.trim().is_empty() || is_property(line)) {
            break;
        }
        props_end += line.len();
    }
    let mut prose: Vec<&str> = preamble[props_end..].lines().collect();
    while prose.last().is_some_and(|line| line.trim().is_empty()) {
        prose.pop();
    }
    if prose.is_empty() {
        return (preamble, None);
    }
    let prose = prose.join("\n");
    preamble.truncate(props_end);
    (preamble, Some(prose))
}

/// Moves any text after the page properties out of `preamble` into a first
/// block at column 0, so bullets indented under it become its children.
fn fold_preamble(preamble: &mut String, roots: &mut Vec<Block>, path: &mut Vec<(usize, usize)>) {
    let (props, prose) = split_preamble(std::mem::take(preamble));
    *preamble = props;
    if let Some(prose) = prose {
        push_bullet(roots, path, (0, 2, prose));
    }
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
    /// file from the outline, so a dropped line is deleted from disk. Page
    /// properties before the first bullet are the preamble, and any other
    /// text there is the first block; after the first bullet, any line that
    /// does not start a new block continues the most recent one.
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
                    fold_preamble(&mut preamble, &mut roots, &mut path);
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

        if open.is_none() {
            fold_preamble(&mut preamble, &mut roots, &mut path);
        }
        Outline { preamble, roots }
    }

    /// Re-parses one block's text the way a reload would. A line in it that
    /// looks like a bullet (outside a fence) can only be stored as a child
    /// block, so it becomes one now, ahead of the block's existing children as
    /// it would be in the file. Returns whether anything moved.
    pub fn split_bullet_lines(&mut self, id: BlockId) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let Some((&last, parent_path)) = path.split_last() else {
            return false;
        };
        let block = &mut children_mut(&mut self.roots, parent_path)[last];
        let alone = Outline {
            preamble: String::new(),
            roots: vec![Block::new(block.text.clone())],
        };
        let mut reparsed = Outline::parse(&alone.serialize()).roots;
        if reparsed.len() != 1 || reparsed[0].children.is_empty() {
            return false;
        }
        let root = reparsed.remove(0);
        block.text = root.text;
        let existing = std::mem::replace(&mut block.children, root.children);
        block.children.extend(existing);
        true
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

    pub fn insert_before(&mut self, id: BlockId, text: impl Into<String>) -> Option<BlockId> {
        let path = self.path_to(id)?;
        let (&last, parent_path) = path.split_last()?;
        let block = Block::new(text);
        let new_id = block.id;
        let siblings = children_mut(&mut self.roots, parent_path);
        siblings.insert(last, block);
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

    /// Every block, parents before their children, in document order. A
    /// block's position here is stable across re-parsing the same text, which
    /// is what lets search name a block before its page is open.
    #[must_use]
    pub fn preorder(&self) -> Vec<&Block> {
        fn walk<'a>(out: &mut Vec<&'a Block>, blocks: &'a [Block]) {
            for block in blocks {
                out.push(block);
                walk(out, &block.children);
            }
        }

        let mut out = Vec::new();
        walk(&mut out, &self.roots);
        out
    }

    /// The blocks in `text` when it is outline markdown of more than one block
    /// (such as a copied bullet with children), or `None` for anything that
    /// should paste as plain text.
    #[must_use]
    pub fn parse_pasted(text: &str) -> Option<Vec<Block>> {
        let pasted = Outline::parse(text);
        let structured = pasted.preamble.is_empty()
            && (pasted.roots.len() > 1 || pasted.roots.iter().any(|b| !b.children.is_empty()));
        structured.then_some(pasted.roots)
    }

    /// Inserts `pasted` as siblings right after `id`. An empty `id` is
    /// replaced by the first pasted block instead: it takes that block's text,
    /// and its children go ahead of any `id` already had.
    pub fn paste_blocks(&mut self, id: BlockId, pasted: Vec<Block>) -> bool {
        let Some(path) = self.path_to(id) else {
            return false;
        };
        let Some((&index, parent_path)) = path.split_last() else {
            return false;
        };
        let siblings = children_mut(&mut self.roots, parent_path);
        let mut pasted = pasted.into_iter();
        let target = &mut siblings[index];
        if target.text.trim().is_empty() {
            if let Some(first) = pasted.next() {
                target.text = first.text;
                let existing = std::mem::replace(&mut target.children, first.children);
                target.children.extend(existing);
            }
        }
        let after = index + 1;
        siblings.splice(after..after, pasted);
        true
    }

    /// The markdown for `id` and its descendants, written as if `id` were a
    /// root — what copying a bullet puts on the clipboard.
    #[must_use]
    pub fn subtree_markdown(&self, id: BlockId) -> Option<String> {
        let path = self.path_to(id)?;
        let (&last, parent_path) = path.split_last()?;
        let mut siblings = &self.roots;
        for &i in parent_path {
            siblings = &siblings[i].children;
        }
        let alone = Outline {
            preamble: String::new(),
            roots: vec![siblings[last].clone()],
        };
        Some(alone.serialize())
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
    fn plain_text_does_not_paste_as_blocks() {
        assert!(Outline::parse_pasted("just words").is_none());
        assert!(Outline::parse_pasted("- one bullet").is_none());
        assert!(Outline::parse_pasted("line one\nline two").is_none());
    }

    #[test]
    fn pasting_into_an_empty_block_replaces_it() {
        let mut o = Outline::parse("- a\n-\n  - kept\n- z\n");
        let empty = o.roots[1].id;
        let pasted = Outline::parse_pasted("- # h1\n  - ## h2\n    - ### h3\n- next\n").unwrap();
        assert!(o.paste_blocks(empty, pasted));
        assert_eq!(
            o.serialize(),
            "- a\n- # h1\n  - ## h2\n    - ### h3\n  - kept\n- next\n- z\n"
        );
        assert_eq!(o.roots[1].id, empty, "the pasted-into block keeps its id");
    }

    #[test]
    fn pasting_into_a_non_empty_block_adds_siblings_after_it() {
        let mut o = Outline::parse("- a\n  - b\n- z\n");
        let b = o.roots[0].children[0].id;
        let pasted = Outline::parse_pasted("- x\n  - y\n").unwrap();
        assert!(o.paste_blocks(b, pasted));
        assert_eq!(o.serialize(), "- a\n  - b\n  - x\n    - y\n- z\n");
    }

    #[test]
    fn subtree_markdown_writes_a_nested_block_as_a_root() {
        let o = Outline::parse("- a\n  - b\n    multi\n    - c\n- d\n");
        let b = o.roots[0].children[0].id;
        assert_eq!(
            o.subtree_markdown(b).as_deref(),
            Some("- b\n  multi\n  - c\n")
        );
    }

    #[test]
    fn non_bullet_lines_are_preserved() {
        let o = Outline::parse("intro\n- a\nnot a bullet\n- b\n");
        assert_eq!(o.roots.len(), 3);
        assert_eq!(o.roots[0].text, "intro");
        assert_eq!(o.roots[1].text, "a\nnot a bullet");
        assert_eq!(o.roots[2].text, "b");
        assert_eq!(o.serialize(), "- intro\n- a\n  not a bullet\n- b\n");
    }

    #[test]
    fn prose_before_the_first_bullet_becomes_the_first_block() {
        let o = Outline::parse("Some intro\n\nmore intro\n\n- a\n");
        assert_eq!(o.preamble, "");
        assert_eq!(o.roots.len(), 2);
        assert_eq!(o.roots[0].text, "Some intro\n\nmore intro");
        assert_eq!(o.roots[1].text, "a");
    }

    #[test]
    fn bullets_indented_under_leading_prose_are_its_children() {
        let o = Outline::parse("# Heading\n\t- child\n- sibling\n");
        assert_eq!(o.roots.len(), 2);
        assert_eq!(o.roots[0].text, "# Heading");
        assert_eq!(o.roots[0].children[0].text, "child");
        assert_eq!(o.roots[1].text, "sibling");
    }

    #[test]
    fn page_without_bullets_is_one_editable_block() {
        let o = Outline::parse("just a paragraph\nand another\n");
        assert_eq!(o.roots.len(), 1);
        assert_eq!(o.roots[0].text, "just a paragraph\nand another");
        assert_eq!(o.serialize(), "- just a paragraph\n  and another\n");
    }

    #[test]
    fn properties_stay_in_the_preamble_and_prose_after_them_is_a_block() {
        let o = Outline::parse("title:: Foo\ntags::\n\nIntro\n- a\n");
        assert_eq!(o.preamble, "title:: Foo\ntags::\n\n");
        assert_eq!(o.roots[0].text, "Intro");
        assert_eq!(o.serialize(), "title:: Foo\ntags::\n\n- Intro\n- a\n");
    }

    #[test]
    fn split_bullet_lines_turns_typed_bullets_into_children() {
        let mut o = Outline::parse("- a\n  - existing\n");
        let a = by_text(&o, "a");
        assert!(o.set_text(a, "a\nmore\n- b\n  - c\n- d"));
        assert!(o.split_bullet_lines(a));
        assert_eq!(o.get(a), Some("a\nmore"));
        let children: Vec<&str> = o.roots[0]
            .children
            .iter()
            .map(|b| b.text.as_str())
            .collect();
        assert_eq!(children, ["b", "d", "existing"]);
        assert_eq!(o.roots[0].children[0].children[0].text, "c");
        assert_eq!(
            o.serialize(),
            Outline::parse(&o.serialize()).serialize(),
            "what is shown is what reloads",
        );
    }

    #[test]
    fn split_bullet_lines_leaves_fenced_bullets_alone() {
        let mut o = Outline::parse("- ```\n  - code\n  ```\n");
        let id = o.roots[0].id;
        assert!(!o.split_bullet_lines(id));
        assert_eq!(o.get(id), Some("```\n- code\n```"));
    }

    #[test]
    fn split_bullet_lines_without_bullets_is_a_noop() {
        let mut o = Outline::parse("- a\n  b\n");
        let id = o.roots[0].id;
        assert!(!o.split_bullet_lines(id));
        assert_eq!(o.get(id), Some("a\nb"));
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
    fn insert_before_creates_sibling_above() {
        let mut o = Outline::parse("- a\n- c\n  - d\n");
        let c = by_text(&o, "c");
        let new_id = o.insert_before(c, "b").unwrap();
        assert_eq!(o.serialize(), "- a\n- b\n- c\n  - d\n");
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
