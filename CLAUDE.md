# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

A scaffolded GPUI application (from Create GPUI App). GPUI is the GUI framework that powers the Zed editor; it is pulled directly from the `zed-industries/zed` Git repository rather than crates.io, so first builds download and compile the full Zed workspace's `gpui` crate and its transitive deps — expect a long initial `cargo build`.

Both `gpui` and `gpui_platform` are pinned to a specific `rev` in `Cargo.toml` (currently `ec9be5c3`). Do **not** remove the pin casually: upstream moves fast and breaks this crate's API (e.g. `Application::new()` was removed in favor of `gpui_platform::application()`). See the README's "GPUI dependency" section for the bump process.

## Project management

Feature planning, tracking, and dependencies live in GitHub Issues on this repo's `origin`. Use the `gh` CLI for all project management: `gh issue list`, `gh issue view <n>`, `gh issue create`, `gh issue edit`, `gh issue close`, etc. Dependencies between issues are expressed in the issue body under a `## Dependencies` section with `Blocked by: #N` and `Blocks: #M` lines — this forms the feature DAG. When picking the next piece of work, prefer issues whose `Blocked by` list is empty or fully closed.

When starting work on an issue, create a new branch linked to that issue (`gh issue develop <n> --checkout`) and, once the work is ready, open a PR against `main` with `gh pr create` — include `Closes #<n>` in the PR body so the issue auto-closes on merge.

## Commands

- If a `justfile` command overlaps with a `cargo` command, use the `justfile` command instead. For instance if both have a `check` command, use `just check` instead of `cargo check`. Run `just --list` to see a list of commands.
- `cargo run` — build and launch the app window.
- `cargo build` / `cargo build --release` — compile only.
- `cargo check` — fast type-check (preferred for quick feedback given the heavy gpui dep).
- `cargo fmt` / `cargo clippy` — formatting and lints.

There are no tests yet; `cargo test` will run zero tests.

## Architecture

Single-binary GPUI app in `src/main.rs`:

- `gpui_platform::application().run(...)` enters the GPUI runtime, giving a `&mut App` context. (The older `Application::new()` entry point no longer exists on current `gpui` HEAD.)
- `cx.open_window(WindowOptions::default(), ...)` opens a window whose root view is constructed via `cx.new(|_cx| ...)` — the closure returns the root view struct (`HelloWorld`).
- A view is any struct implementing `Render`. `Render::render` returns an `impl IntoElement` built from the `div()` element builder with chained style/layout methods (`.flex()`, `.bg()`, `.size_full()`, etc.) and `.child(...)` for content.
- `SharedString` is GPUI's cheap-clone string type used for view-owned text.

When extending: add new views as structs implementing `Render`, compose them as children of the root, and use `cx.new(...)` inside event handlers or setup to instantiate child views with their own state. `smallvec` is pre-added because GPUI uses it for variadic child lists.
