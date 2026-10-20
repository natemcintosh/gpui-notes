# Fixtures

## `kitchen-sink.md`: manual QA of block rendering

One block per feature the renderer supports, each saying what it should look like. `tests/kitchen_sink.rs` checks that the file round-trips through a save unchanged and still lowers to every feature it lists, so add a block there when the renderer learns something new.

To check it by eye, open it as a page in a throwaway notes root:

```sh
root=$(mktemp -d) && mkdir "$root/pages" && cp docs/fixtures/kitchen-sink.md "$root/pages/" \
  && GPUI_NOTES_ROOT="$root" cargo run --release
```

Open `kitchen-sink` from the page picker, then for each block:

1. In view mode, it looks the way its text says.
2. Clicking it opens the editor with the raw markdown, and Escape renders it again unchanged.
3. Links do what the block says on click (the browser, this page, or the 2026-07-13 journal).

Resize the window narrow to check that the long paragraph wraps. After quitting, `diff docs/fixtures/kitchen-sink.md "$root/pages/kitchen-sink.md"` should print nothing.

## Performance baseline

`redraw_1000_plain_blocks_baseline` in `src/outline_view.rs` times a full redraw of a page of 1000 one-line blocks, which is what every keystroke costs. It is ignored by default; run it with:

```sh
cargo nextest run --release --run-ignored only --no-capture redraw_1000
```

It runs headless, so it covers render and layout but not GPU time. The target is under 16 ms per frame.

| Date | Commit | Machine | Release | Debug |
| --- | --- | --- | --- | --- |
| 2026-10-19 | `5b2aa31` | Linux x86_64 | 28 ms | 226 ms |

The release figure misses the target. #76 tracks the per-keystroke re-render work.
