# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

A scaffolded GPUI application (from Create GPUI App). GPUI is the GUI framework that powers the Zed editor; it is pulled directly from the `zed-industries/zed` Git repository rather than crates.io, so first builds download and compile the full Zed workspace's `gpui` crate and its transitive deps — expect a long initial `cargo build`.

Both `gpui` and `gpui_platform` are pinned to a specific `rev` in `Cargo.toml` (currently `ec9be5c3`). Do **not** remove the pin casually: upstream moves fast and breaks this crate's API (e.g. `Application::new()` was removed in favor of `gpui_platform::application()`). See the README's "GPUI dependency" section for the bump process.

## Project management

Issues, branches, and pull requests are optional for routine work. Do not create an issue or a feature branch by default; work directly on `main` unless the user explicitly asks for a separate branch or PR.

When work is already tracked in GitHub Issues, use the `gh` CLI for project management. Dependencies between issues are expressed in the issue body under a `## Dependencies` section with `Blocked by: #N` and `Blocks: #M` lines. When fetching tracked work, start with `just unblocked`, which lists issues with no open blockers.

## Commands

- If a `justfile` command overlaps with a `cargo` command, use the `justfile` command instead. For instance if both have a `check` command, use `just check` instead of `cargo check`. Run `just --list` to see a list of commands.
- `cargo run` — build and launch the app window.
- `cargo build` / `cargo build --release` — compile only.
- `cargo check` — fast type-check (preferred for quick feedback given the heavy gpui dep).
- `cargo fmt` / `cargo clippy` — formatting and lints.
- `just check` runs `cargo clippy --all-targets -- -D warnings -W clippy::pedantic`, mirroring CI's clippy step exactly. It must pass cleanly before commit — CI treats any clippy warning as an error, and `--all-targets` means test-only code (helpers, fixtures) is linted too. Pedantic findings surface as warnings: fix them in touched code (restructure to avoid the lint when possible, otherwise suppress locally with `#[allow(clippy::lint_name)]` and a one-line reason).
- `just test` runs `cargo nextest run --no-fail-fast` — the test suite. CI uses [nextest](https://nexte.st/), so local runs should too. Install with `cargo install cargo-nextest --locked` if not already present.
- `just pre` runs `prek run --all-files` (pre-commit hooks).

Tests use GPUI's built-in test framework. `gpui` is added as a `[dev-dependencies]` entry with the `test-support` feature enabled (same git/rev pin as the main dep — keep them in sync when bumping). `tempfile` (temp notes roots) and `rstest` (parameterized cases) are also dev-deps.

Write tests with the `#[gpui::test]` attribute macro (in place of `#[test]`). The macro injects a headless `TestAppContext` (or `TestVisualContext` for window-level tests), which drives a simulated platform — no real GPU/window is needed. Typical shape:

```rust
use gpui::{AppContext, TestAppContext};

#[gpui::test]
fn it_works(cx: &mut TestAppContext) {
    let view = cx.new(|_| HelloWorld { text: "hi".into() });
    cx.read_entity(&view, |v, _| assert_eq!(v.text.as_ref(), "hi"));
}
```

`TestAppContext` exposes `new`, `update`, `read`, `executor()` / `foreground_executor()` for driving async tasks, and helpers for simulating keystrokes, mouse events, and modifiers. Use `cx.run_until_parked()` (via the executor) to flush pending effects. For multi-client scenarios, `cx.new_app()` spawns a second context sharing the same executor. Run with `cargo nextest run`.

### Testing UI interactions

For any code path that responds to a click, keystroke, or focus event, prefer the simulation APIs over reaching directly into the view. `VisualTestContext` exposes `simulate_click(point, modifiers)`, `simulate_mouse_down/up`, `simulate_keystrokes("escape")`, and `simulate_input("hello")` — all of which drive the same event-dispatch path the real platform uses. Programmatic shortcuts like `window.focus(&handle)` or calling `view.update(...)` to invoke a handler directly skip large parts of that path (notably the mouse-event listeners that gpui auto-registers on focusable elements via `track_focus`), so a programmatic test can pass while the actual click is broken.

Concrete rule of thumb: if the production trigger is a mouse click, the test should call `simulate_click`; if it's a keystroke, `simulate_keystrokes`. Use `window.focus` in tests only to set up an unrelated precondition (e.g. "start with block X focused, now test the keystroke behavior"). When a test needs key bindings to be active, call `text_input::bind_keys(cx)` (or any other module's `bind_keys`) inside the `add_window_view` closure — bindings registered in `main` don't exist in the headless runtime.

This wasn't always followed: the per-block edit/view toggle landed with tests that called `window.focus(&bv_handle)` to enter editing, which silently bypassed the `track_focus` auto-focus that was actually breaking real clicks. The regression tests in `src/block_view.rs` (`click_focuses_text_input`, `escape_returns_to_focused_viewing`) show the pattern to follow.

## Architecture

As much as possible, use rust's rich type system to encode state and make invalid states impossible (e.g. `OverlayMode` in `main.rs` makes "two overlays open at once" unrepresentable).

An outliner-style notes app (pages of nested markdown blocks, `#tags`, `[[Page]]` links, daily journals). Lib crate `gpui_notes` (`src/lib.rs`) holds the modules; `src/main.rs` is the binary with `RootView` (top-level view + overlay state machine) and app startup.

- **Storage**: `store.rs` — `NotesStore` reads/writes markdown files under a notes root: `pages/<Name>.md` and `journals/YYYY-MM-DD.md`.
- **Model**: `outline.rs` (`Outline`/`Block`/`BlockId` — the parsed block tree), `page.rs` (`Page` entity: name + outline + dirty flag + its `OutlineView`), `journal.rs` (open-by-date helpers).
- **Globals** (`cx.global`): `PageRegistry` (open-page cache, save/autosave), `CurrentPage`, `NavHistory` (back/forward stacks), `TagIndex`, `LinkIndex`, `LastError` (dismissible error line, `errors::report`).
- **Navigation**: `history.rs` — every user-initiated page change goes through `history::navigate_to`, the one place that maps a target onto `registry::set_current_page` / `journal::open_for_date` and records the outgoing page. Do not call those two primitives directly from a handler, or the jump becomes invisible to back/forward. Startup is the deliberate exception (`main` calls `journal::open_today`) so the back stack begins empty. History entries are `TagSource`, not `Entity<Page>`, because a page named `2026-07-13` and that date's journal are distinct (#40). `history::go(NavigationDirection)` steps back/forward; it's bound to `alt-left`/`alt-right` and to the mouse side buttons via `MouseButton::Navigate` on `RootView`'s root element.
- **Views**: `WindowFrame` (client-side chrome on compositors without server decorations) → `RootView` → the current page's `OutlineView` → one `BlockView` per block (rendered markdown via `block_render.rs`, toggling to a `TextInput` editor on Enter) + `ShortcutBar`. Overlays (`PagePicker`, shortcuts help, tag results) live in `RootView`'s `OverlayMode` enum.
- **Linked references**: `linked_refs.rs` renders the "N Linked References" section beneath a page's own blocks — every block elsewhere that contains a `[[This Page]]` link, plus its descendants, grouped by source (journals newest first, then pages alphabetically). The rows are real `OutlineView`s in `OutlineScope::Subtree`, so a reference is **editable in place** and the edit lands in the source `Page`; that is also why `ctrl-s` saves *all* open pages, not just the current one. `OutlineView` routes every traversal through `scoped_roots`, which is what keeps focus moves and backspace inside a reference. Only `OutlineScope::Page` owns a scroll container (the refs section is its last child, so `scroll_to_item` indices stay aligned with `flatten_visible`) and only it mounts a `LinkedRefsView` — lazily, because `Page::new` builds its `OutlineView` from `cx.entity()` before the page exists. `LinkIndex` records only *which* `TagSource`s reference a target, never `BlockId`s: ids minted by `rebuild_from`'s throwaway parse do not survive into the `Page` opened later, so `page_links::blocks_linking_to` resolves them from the live outline. That resolution opens and walks every referencing page, so the grouping is **cached** in `LinkedRefsView` and rebuilt only when invalidated — the `LinkIndex` observer covers the *set* of sources, one `cx.observe` per opened source `Page` covers the blocks *within* a source (which is what puts an in-place edit back on screen), and `render` only walks the cache, because GPUI re-renders the whole window on every keystroke. Only the first `REFS_PER_PAGE` (20) references are mounted; the rest wait behind a "Show more" row, since each one is a live `OutlineView` plus a `BlockView` per block. `LinkIndex` indexes journals as well as pages (that is where most references are written), but journals are deliberately kept out of its `pages` set, which drives missing-page styling and the `[[` completion list.
- **Multi-line blocks**: a `Block`'s text may span lines. `outline.rs` treats a non-bullet line indented to at least the bullet's text column as a continuation (bullet-looking lines inside an open ``` fence included), and re-indents them on `serialize`. In the editor, Enter inserts a newline once the block is multi-line or has an open fence; `shift-enter` is the always-new-sibling escape hatch. `calc.rs` evaluates ```calc fences at render time — display only, never written back.
- **Inline completion**: typing `[[` or `#` mid-edit opens a popup under the caret. `page_completion.rs` is the pure model (trigger scanning, candidate ranking, the "New page/tag" row); `BlockView` owns the state inside its `Editing` variant, refreshes it from `cx.observe` on the `TextInput` (caret moves notify without changing text), and renders it as `deferred(anchored())` positioned at `TextInput::caret_bounds()`. **It adds no key bindings**: Enter/Escape are intercepted in the existing `TextInputEvent::{Submitted, Cancelled}` handlers and Up/Down in `move_caret_vertical`, because GPUI resolves the deepest matching context first and `text_input.rs` already binds those keys. `[[` auto-pairs to `[[|]]`, driven from `Changed` (never from the popup refresh) so parking the caret inside an existing link cannot insert brackets.
- **Key bindings**: each interactive module exposes `bind_keys(cx)`; `main` calls them all, tests must re-call them in `add_window_view`. `lib.rs::cmd_key()` returns the platform modifier (`cmd`/`ctrl`). `shortcut_hints.rs` discovers active bindings from the dispatch tree at runtime — new bindings appear in the UI automatically. Bindings that must not fire mid-edit need the `!TextInput` context predicate (GPUI matches predicates against the whole focus path); note that `text_input.rs`'s own bindings are partly `cfg`-gated per platform, so a conflict you see on one OS may not exist on the other.
- `text_input.rs` is ported from Zed's `gpui/examples/input.rs` at the pinned rev — when bumping the rev, diff against that file first.

GPUI basics: views are structs implementing `Render`, built from the `div()` element builder; child views are created with `cx.new(...)`; `SharedString` is the cheap-clone string type for view-owned text.

`AGENTS.md` is a symlink to this file — edits here cover both.
