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
- **Globals** (`cx.global`): `PageRegistry` (open-page cache, save/autosave), `CurrentPage`, `TagIndex`, `LinkIndex`, `LastError` (dismissible error line, `errors::report`).
- **Views**: `WindowFrame` (client-side chrome on compositors without server decorations) → `RootView` → the current page's `OutlineView` → one `BlockView` per block (rendered markdown via `block_render.rs`, toggling to a `TextInput` editor on Enter) + `ShortcutBar`. Overlays (`PagePicker`, shortcuts help, tag results) live in `RootView`'s `OverlayMode` enum.
- **Key bindings**: each interactive module exposes `bind_keys(cx)`; `main` calls them all, tests must re-call them in `add_window_view`. `lib.rs::cmd_key()` returns the platform modifier (`cmd`/`ctrl`). `shortcut_hints.rs` discovers active bindings from the dispatch tree at runtime — new bindings appear in the UI automatically.
- `text_input.rs` is ported from Zed's `gpui/examples/input.rs` at the pinned rev — when bumping the rev, diff against that file first.

GPUI basics: views are structs implementing `Render`, built from the `div()` element builder; child views are created with `cx.new(...)`; `SharedString` is the cheap-clone string type for view-owned text.

`AGENTS.md` is a symlink to this file — edits here cover both.
