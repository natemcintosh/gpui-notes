# gpui-notes

A Rust + [GPUI](https://www.gpui.rs/) note-taking app inspired by [Logseq](https://github.com/logseq/logseq). It is not aiming at full Logseq parity — the goal is a focused subset of the features that get used day-to-day. GPUI is the GUI framework that powers the [Zed](https://github.com/zed-industries/zed) editor.

## Planned features

- **Daily journal pages** — an auto-created dated page as the default capture surface.
- **Markdown editing with rendered preview** — blocks render as formatted markdown when not focused, and switch to raw markdown on edit.
- **Outliner / block-based editing** — every line is a collapsible, nestable bullet block.
- **Bidirectional page links & backlinks** — `[[Page Name]]` syntax auto-creates pages; each page shows references back to it.
- **Block references** — `((block-id))` transcludes a specific block; edits sync everywhere it appears.
- **`#tags`** for categorizing blocks and pages. (No plan to support Logseq's `key:: value` property syntax.)
- **Local-first plain-markdown storage** — notes are saved as plain `.md` files on disk, syncable via Git, Dropbox, iCloud, or any other file sync.

Not currently planned (may be revisited later): graph view, query blocks, task states, flashcards, PDF annotation, whiteboards, and a plugin ecosystem.

## GPUI dependency

GPUI is not published to crates.io. We depend on it (and `gpui_platform`) directly from the [`zed-industries/zed`](https://github.com/zed-industries/zed) repository. Because that repo is Zed's active development trunk, its API changes without warning — the scaffold emitted by `create-gpui-app` has already drifted from HEAD at least once (e.g. `Application::new()` was removed in favor of `gpui_platform::application()`).

To keep builds reproducible, we **pin both crates to a specific `rev`** in `Cargo.toml`. Rules of the road:

- Never depend on the floating `main` branch. Always use `rev = "..."`.
- Both `gpui` and `gpui_platform` must share the same `rev`, or Cargo will pull two copies of the Zed workspace.
- To bump: pick a new commit from `zed-industries/zed`, update both `rev` values, run `cargo update -p gpui -p gpui_platform`, then `cargo check`. Expect to fix API breakage; check the relevant example in `zed/crates/gpui/examples/` for the current idiomatic usage.
- Commit the resulting `Cargo.lock` change alongside the `Cargo.toml` bump.
