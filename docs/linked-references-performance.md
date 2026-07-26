# Linked references — performance work

Follow-up to `docs/linked-references.md`. The feature is correct, but opening a
heavily-referenced page in a real vault slows the app to a crawl.

## Measurements that motivate this

Real vault (`~/Desktop/logseq-test-zone`): 136 pages, 1069 journals, ~42k lines.
Reference counts for the worst targets:

| Target | `[[…]]` occurrences |
| --- | --- |
| `What would make today great?` / `What am I worried about?` / `What am I grateful for?` / `morning pages` | 209 each |
| `sub-state Rt` | 159 |
| `NSSP` | 150 |
| `Rt` | 148 |
| `meeting notes` | 109 |

## Why it is slow

Nothing in the refs section is cached, and its render is O(whole vault):

1. `LinkedRefsView::render` (`src/linked_refs.rs:158-166`) rebuilds the entire
   grouping every render. `referencing_blocks` → `page_links::blocks_linking_to`
   → `page_links_in_text`, which is a **full pulldown-cmark parse**
   (`block_render::lower`, `src/block_render.rs:89`) of *every block of every
   referencing page*. Opening `NSSP`: ~150 sources × ~40 blocks ≈ 6000 markdown
   parses per render.
2. That render fires on **every keystroke**: `TextInputEvent::Changed`
   (`src/block_view.rs:224-229`) → `set_block_text` → `Page` notify →
   `OutlineView`'s `cx.observe(&page)` → notify → render, which mounts
   `refs.clone()` as a child element. A GPUI `Entity` used as an element
   re-renders whenever its parent does (nothing here is `.cached()`). The
   `observe_global::<LinkIndex>` at `src/linked_refs.rs:44` is a second trigger.
3. Every referencing block is a live `OutlineView` + one `BlockView` per block
   and descendant, and `BlockView::render` (`src/block_view.rs:568-591`)
   re-lowers markdown via `render_block` each time. No virtualization: hundreds
   of extra mounted views, all re-rendering together, each also holding
   `observe_global::<LinkIndex>` (`src/block_view.rs:119`).

## Work items

### 1. Compute `groups` outside `render`

Hold the grouping in `LinkedRefsView` state and rebuild it only when it can
actually have changed:

- the `LinkIndex` observer fires (`src/linked_refs.rs:44`), or
- the target page changes.

`render` then only walks the cached `groups` and emits elements. This removes
the per-keystroke recompute, which is the dominant cost.

Watch out for: the referencing block ids are deliberately *not* stored in
`LinkIndex` (see `docs/linked-references.md` — ids from `rebuild_from`'s
throwaway parse do not survive into the opened `Page`), so the cache must be
keyed on something that is invalidated when a source page's outline changes.
Observing each open source `Page` entity, or stamping the cache with the source
outline's revision, are both workable; editing a reference in place must still
update the list.

Verify: existing `linked_refs.rs` tests still pass; add one asserting that
typing into a block on the *current* page does not rebuild the grouping (e.g. a
recompute counter, or assert `groups()` returns an unchanged snapshot).

### 2. Prescan before parsing in `blocks_linking_to`

`src/page_links.rs::blocks_linking_to` currently runs the full markdown lowering
on every block. Skip blocks whose raw text cannot contain a link — match against
`PAGE_LINK_RE` (or just `text.contains("[[")`) first, and only run
`page_links_in_text` on the survivors. The parse is still needed for the
survivors, because the fence-exclusion rule ("a `[[Target]]` inside a ``` fence
is not a reference") depends on it.

Typical journals have links in a small minority of blocks, so this should cut
the parse count by ~1-2 orders of magnitude.

Verify: `blocks_linking_to_finds_nested_blocks_and_skips_code` must still pass
unchanged — it is exactly the fenced-link case the prescan must not break.

### 3. Cap what gets mounted

Even with a perfect cache, mounting 150-209 editable `OutlineView`s is
inherently expensive. Options, in preference order:

- Render the first N references (N ≈ 20) with a "Show more" row.
- Or render groups collapsed by default (closer to Logseq's behaviour), mounting
  a group's subtree views only when it is expanded.

Either way `LinkedRefsView.refs` should only hold views for what is currently
shown, and the existing pruning (`self.refs.retain(...)`) needs to keep working
so an in-progress edit is not dropped when the visible set changes.

Verify: a test that a page with many references mounts only the capped number of
subtree views, plus the existing edit-in-place test still passing for a visible
reference.

## Related, deliberately not in this list

- `referencing_blocks` calls `errors::report` from inside `render`
  (`src/linked_refs.rs:96`). That is `update_global::<LastError>`, which
  `RootView` observes (`src/main.rs:118`) → notify → re-render → same failure →
  report again: an unbounded render loop for any source that fails to open. All
  sources currently exist on disk, so it is latent, but page-opening and error
  reporting should both leave the render path.
- Opening sources during render also permanently populates `PageRegistry`
  (`src/registry.rs:196` — each `insert` parses the file, builds a `Page` *and*
  its `OutlineView`, and fires two global notifies). Those pages stay open, so
  `ctrl-s`'s `save_all` and every later global notify scale with them too.
