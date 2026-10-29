# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. It holds the rules and traps that aren't visible from a single file; how each part works is in its module's doc comments.

## Project

A Logseq-compatible outliner built on GPUI, the GUI framework behind the Zed editor. GPUI comes from the `zed-industries/zed` Git repository, not crates.io, so the first build compiles a large part of Zed's workspace and takes a long time.

`gpui` and `gpui_platform` are pinned to a `rev` in `Cargo.toml` (currently `ec9be5c3`). Do **not** remove the pin casually: upstream moves fast and breaks this crate's API (e.g. `Application::new()` was removed in favor of `gpui_platform::application()`). See the README's "GPUI dependency" section for the bump process.

## Project management

Issues, branches, and pull requests are optional for routine work. Do not create an issue or a feature branch by default; work directly on `main` unless the user explicitly asks for a separate branch or PR.

When work is already tracked in GitHub Issues, use the `gh` CLI for project management. Dependencies between issues are expressed in the issue body under a `## Dependencies` section with `Blocked by: #N` and `Blocks: #M` lines. When fetching tracked work, start with `just unblocked`, which lists issues with no open blockers.

## Commands

Prefer a `just` recipe over the overlapping `cargo` command (`just --list`).

- `just check` runs `cargo clippy --all-targets -- -D warnings -W clippy::pedantic`, exactly as CI does. It must pass before committing; `--all-targets` means test code is linted too. Fix pedantic findings in touched code (restructure to avoid the lint when possible, otherwise suppress locally with `#[allow(clippy::lint_name)]` and a one-line reason).
- `just test` runs `cargo nextest run --no-fail-fast`, as CI does. Install with `cargo install cargo-nextest --locked` if missing.
- `just pre` runs the pre-commit hooks (`prek run --all-files`).
- `cargo check` is the quickest feedback, given the heavy gpui dependency.
- `just run` resets `~/Desktop/logseq-test-zone` from `~/logseq` and runs the app against that copy. `just real` runs against the real graph and saves into the user's actual notes; don't use it unless asked.
- `just release <major|minor|patch>` bumps the version, commits, tags `v<version>` and **pushes**; CI then builds the binaries. Don't run it unless asked. `.github/workflows/release.yml`'s asset names must match `[package.metadata.binstall]` in `Cargo.toml`, or `cargo binstall` can't find them.

## Testing

Write tests with `#[gpui::test]`, which injects a headless `TestAppContext` (or `VisualTestContext` for a window). `gpui` is also a dev-dependency with the `test-support` feature, on the same git `rev` as the main dependency: keep the two in sync when bumping. `tempfile` (temp notes roots) and `rstest` (parameterized cases) are dev-deps too.

- **Drive UI through the real event path.** If the production trigger is a mouse click, the test calls `simulate_click` (or `simulate_mouse_down/up`); if it's a keystroke, `simulate_keystrokes`. Calling `window.focus(...)` or a handler directly skips parts of the dispatch path, notably the focus listeners `track_focus` installs, so the test can pass while the real click is broken. That happened with the per-block edit/view toggle. Use `window.focus` only to set up an unrelated precondition. `click_focuses_text_input` and `escape_returns_to_focused_viewing` in `block_view.rs` show the pattern.
- **Bind keys in the test.** Bindings registered in `main` don't exist headless: call each needed module's `bind_keys(cx)` inside the `add_window_view` closure.
- **Headless is not the real app.** `request_animation_frame` callbacks never run there (use a spawned task, as `journals_view.rs` does). Text also wraps differently, so a layout fix (heights, overlap, wrapping) can pass every test and still be broken: check it in the real app before calling it fixed. Temporarily logging bounds with `.on_children_prepainted(...)` helps, but text that fails to wrap still looks fine in those logs, so ask the user to look. Percentage widths in nested containers caused the journals overlap; a definite pixel width fixed it.

## Architecture

Use Rust's type system to make invalid states unrepresentable (e.g. `OverlayMode` in `main.rs` makes "two overlays open at once" impossible to express).

The lib crate (`src/lib.rs`) holds the modules; `src/main.rs` has `RootView` and startup. `#tags` were removed in favour of links, so `#foo` is plain text.

- **Pages vs journals**: files are `pages/<Name>.md` and `journals/YYYY_MM_DD.md`, but journal *titles* are `YYYY-MM-DD`, as in Logseq. A page named `2026-07-13` and that day's journal are different notes (#40), so navigation and history use `NoteSource`/`NavTarget`, never `Entity<Page>`, and a `[[2026-07-13]]` link opens the journal (`page_links::link_target`). Journals mode lists only days with a file (plus today) and never creates files for the gaps.
- **A save rewrites the whole file from the outline**, so `outline.rs` must never drop a line: page properties, text before the first bullet, continuation lines and blanks all have to round-trip. Anything computed for display (```` ```calc ```` results) is never written back.
- **Navigation**: every user-initiated page change goes through `history::navigate_to`, including zooming in on a bullet (`NavTarget::Block`; views without access to `RootView` dispatch `OpenTarget`). Calling `registry::set_current_page`, `journal::open_journals` or `OutlineView::set_zoom` from a handler makes the jump invisible to back/forward. Startup is the one exception, so the back stack starts empty.
- **Undo**: every outline mutation on `Page` goes through `apply_outline_edit` or `set_block_text`; anything else is invisible to undo.
- **Several pages are live at once**: journals mode shows many days, and linked references are edited in place in their *source* page. So `ctrl-s` and leaving journals mode save *all* open pages, not just the current one.
- **`render` runs on every keystroke** (GPUI re-renders the whole window), so expensive work is cached and recomputed only when its inputs change: linked-reference resolution in `LinkedRefsView`, misspelled ranges in `TextInput`.
- **Block rendering**: `block_render.rs` renders each paragraph as **one shaped `StyledText`**. Do *not* go back to a div per inline span: each span became a shrinkable flex item, and gpui's text measure cache kept re-confirming a narrow width, collapsing any wrapping block into a one-character column. So an `InlineExtension` returns an `InlineRun`, never an element (a link can't have a border or a hover cursor); mouse-*down* on a clickable range must `stop_propagation()`, or `BlockView` swaps in the editor before the click completes; and every run names its colour explicitly, because a wrapper's `text_color` arrives after the runs are built and an inherited colour comes out black (`every_run_carries_a_theme_colour` guards this).
- **Outline scopes**: `OutlineView` routes every traversal through `scoped_roots`, which keeps focus moves and backspace inside a linked reference. Only `OutlineScope::Page` owns a scroll container (journals scroll in `JournalsView`), so reveals go through `autoscroll`, never a parent's `ScrollHandle` index: the caret reveals itself when it moves, and keyboard focus moves set `reveal_row`.
- **Blocks found by parsing files** (`LinkIndex`, search hits) are identified by source and preorder position, never by `BlockId`: ids from a throwaway parse don't survive into the `Page` opened later.
- **Editor popups add no key bindings.** GPUI resolves the deepest context first and `text_input.rs` already binds Enter, Escape and Up/Down, so the `[[` completion intercepts them in `BlockView`'s existing `TextInputEvent` handlers and `move_caret_vertical`. For the same reason, block paste works because `BlockView` *captures* `Paste` before `TextInput`'s own paste flattens newlines.
- **Key bindings**: each interactive module exposes `bind_keys(cx)`, which `main` calls. Use `cmd_key()` rather than hardcoding `cmd` or `ctrl`. Bindings that must not fire mid-edit need `!TextInput` in their context (predicates match the whole focus path). `text_input.rs`'s own bindings are partly `cfg`-gated per platform, so a conflict seen on one OS may not exist on another. The shortcut overlay reads bindings from the dispatch tree at runtime, so there's no list to maintain.
- **Spell check** covers only the block being edited; rendered blocks are never checked.
- **Per-machine settings** live in `config::config_dir()` (`~/.config/gpui-notes` on Linux and macOS, `%APPDATA%\gpui-notes` on Windows), never in the notes root. Add a new setting as a `Config` field in `config.toml`, not as a new file. Debug builds ignore `config.toml`, so a dev `cargo run` can't write to the graph an installed copy points at; use `GPUI_NOTES_ROOT` to point it elsewhere.
- **Colours**: views call the `theme::` functions, which read the active palette. Never cache a colour across renders or hardcode one in a view, or switching themes misses it.
- `text_input.rs` is ported from Zed's `gpui/examples/input.rs` at the pinned rev; when bumping the rev, diff against that file first.

`AGENTS.md` is a symlink to this file — edits here cover both.
