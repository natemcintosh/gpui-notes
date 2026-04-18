# gpui-notes

Created with Create GPUI App.

- [`gpui`](https://www.gpui.rs/)
- [GPUI documentation](https://github.com/zed-industries/zed/tree/main/crates/gpui/docs)
- [GPUI examples](https://github.com/zed-industries/zed/tree/main/crates/gpui/examples)

## Usage

- Ensure Rust is installed - [Rustup](https://rustup.rs/)
- Run your app with `cargo run`

## GPUI dependency

GPUI is not published to crates.io. We depend on it (and `gpui_platform`) directly from the [`zed-industries/zed`](https://github.com/zed-industries/zed) repository. Because that repo is Zed's active development trunk, its API changes without warning — the scaffold emitted by `create-gpui-app` has already drifted from HEAD at least once (e.g. `Application::new()` was removed in favor of `gpui_platform::application()`).

To keep builds reproducible, we **pin both crates to a specific `rev`** in `Cargo.toml`. Rules of the road:

- Never depend on the floating `main` branch. Always use `rev = "..."`.
- Both `gpui` and `gpui_platform` must share the same `rev`, or Cargo will pull two copies of the Zed workspace.
- To bump: pick a new commit from `zed-industries/zed`, update both `rev` values, run `cargo update -p gpui -p gpui_platform`, then `cargo check`. Expect to fix API breakage; check the relevant example in `zed/crates/gpui/examples/` for the current idiomatic usage.
- Commit the resulting `Cargo.lock` change alongside the `Cargo.toml` bump.
