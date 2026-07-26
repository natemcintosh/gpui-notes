# Linked references

Implementation summary for the "N Linked References" section — the backlinks half of
the README's *Bidirectional page links & backlinks* feature.

## Problem

Opening a page reached by clicking a `[[Page Name]]` link showed only the text written
*on* that page. Logseq additionally lists, beneath the page body, every block elsewhere
that references it — the referencing block plus its whole child subtree, grouped under
its source page or journal, and editable in place.

## Root cause

The data layer largely existed but had never had a consumer, and it was wrong in three
ways:

1. **Journals were not indexed.** `LinkIndex::rebuild_from` walked only `store.list()`,
   and `registry.rs` guarded the reindex with `if matches!(key, PageKey::Page(_))`. Since
   journals are where most references get written, the common case was invisible — this
   was the actual reported bug.
2. **`Backlink` was `(SharedString, BlockId)`** — no `TagSource`, so a page named
   `2026-07-23` and that date's journal were indistinguishable as sources. Worse, the
   `BlockId`s came from throwaway `Outline::parse` calls inside `rebuild_from` and did not
   match the ids of the `Page` entity opened later.
3. **No way to render another page's block subtree.** `OutlineView` always rendered a
   whole page and always owned its own scroll container.

`LinkIndex::backlinks()` had zero non-test callers before this change.

## Design

**Editable in place, by reusing the real view stack.** A `BlockView` is already bound to
an `(Entity<Page>, BlockId)` pair rather than to an `OutlineView`, so mounting one for
another page's block just works: edits route through `Page::set_block_text`, dirty that
page, and the registry saves it. No read-only renderer was needed and no cross-page edit
plumbing had to be invented.

**A scoped `OutlineView` instead of a new renderer.** One new enum —
`OutlineScope::{Page, Subtree(BlockId)}` — makes a view render a single block subtree.
Every existing action handler (indent, outdent, move, collapse, focus up/down, backspace)
keeps working against the source page for free.

**The index stores sources, not block ids.** `LinkIndex.reverse` became
`HashMap<String, BTreeSet<TagSource>>`. Referencing block ids are resolved from the live
`Page` at render time via `page_links::blocks_linking_to`, which deletes the stale-id
problem outright rather than working around it.

**One scrollbar.** The references section renders as the *last child inside* the
page-scope `OutlineView`'s existing scroll container, so page and references scroll
together and `move_edit_to_adjacent`'s `scroll.scroll_to_item(index)` keeps its index
alignment with `flatten_visible`.

## Changes

| File | Change |
| --- | --- |
| `src/page_links.rs` | `reverse` keyed by `TagSource`; `forward` keyed by `TagSource` (a `BTreeMap`, so a page named `2026-07-23` cannot purge that date's journal); `rebuild_from` walks journals too; `reindex_global_for_page` takes a `&TagSource`; new `blocks_linking_to(&Outline, target)`. |
| `src/registry.rs` | Dropped the `PageKey::Page(_)` guards in `save` and `insert` so journals reindex their links. |
| `src/outline_view.rs` | `OutlineScope` + `subtree()` constructor + `scoped_roots()`; `Render`, `move_edit_to_adjacent`, `move_focus`, `delete_backward`, and `focus_first_block` all route through it; only `Page` scope scrolls and only it mounts the refs section (lazily). |
| `src/linked_refs.rs` *(new)* | `LinkedRefsView`: groups, orders, and renders the references; owns one subtree `OutlineView` per referencing block. |
| `src/main.rs` | `ctrl-s` switched from `reg.save(current)` to `reg.save_all` — an edited reference belongs to a different page than the one on screen. |
| `src/tags.rs` | `TagSource` derives `Hash` (it is now a `HashMap` key). |

### Behaviour details

- Grouping order is journals **newest first**, then pages alphabetically. `TagSource`'s
  derived `Ord` sorts the other way, so `LinkedRefsView::ordered_sources` supplies an
  explicit comparator.
- A page's own blocks linking to itself are excluded — a self-link is not a reference.
- With no references, the section renders nothing at all rather than an empty header.
- Source labels navigate via `history::navigate_to`, never `registry::set_current_page`,
  so back/forward stays correct.
- Journals are indexed but deliberately kept **out of** `LinkIndex.pages`, which drives
  missing-page styling and the `[[` completion list; leaking dates in would have made
  `2026-07-23` an autocomplete candidate.

### Convergence note

The first render of the section opens each referencing page through the registry, which
reindexes it and notifies `LinkIndex`, scheduling one extra render pass. That pass hits
the `refs` cache and settles. Tests call `run_until_parked` after mounting for this reason.

## Tests

13 new tests; 349 total pass.

- `page_links.rs` — journals indexed as backlink sources; journals absent from
  `page_names()`/`page_exists`; a same-named page and journal do not purge each other;
  `blocks_linking_to` finds nested blocks and skips links inside ``` fences.
- `registry.rs` — saving a *journal* refreshes the link index.
- `outline_view.rs` — subtree scope renders only that block and its descendants; `down`
  on the last descendant does not escape into the next root block.
- `linked_refs.rs` — grouping and ordering; the block *and its children* render; clicking
  a source label navigates; a self-link is not listed; and **editing a reference writes
  back to its source page**, driven with `simulate_click` + `simulate_input` per the
  UI-interaction rule in `CLAUDE.md` (a `window.focus` shortcut would have bypassed the
  `track_focus` mouse path that actually matters here).
- `main.rs` — end-to-end reproduction of the reported bug: write `[[NSSP ETL failures]]`
  into today's journal, open that page, assert the reference appears under the body.

`debug_selector` / `debug_bounds` are used to locate real painted bounds for the click
tests; `debug_selector` compiles away outside test builds.

## Verification

- `just check` (clippy pedantic, `-D warnings`, `--all-targets`) — clean
- `just test` — 349 passed
- `just pre` — clean
- Not done: a manual GUI run against a real vault.

## Follow-ups (deliberately out of scope)

- The `#tag` results overlay still uses flat one-line-per-hit rows
  (`RootView::render_tag_results`). The subtree `OutlineView` is directly reusable there.
- Unlinked references and the reference filter (the funnel icon in Logseq).
- Per-reference breadcrumbs showing the ancestor chain.
- The header dirty marker still reflects only the current page, so editing a reference
  shows no `•`.
- `README.md` still lists backlinks under **Planned features**; the backlinks half now
  ships, but block references (`((block-id))`) on the same line do not.
