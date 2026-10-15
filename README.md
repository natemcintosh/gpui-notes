# gpui-notes

A Rust + [GPUI](https://www.gpui.rs/) note-taking app inspired by [Logseq](https://github.com/logseq/logseq). It is not aiming at full Logseq parity — the goal is a focused subset of the features that get used day-to-day. GPUI is the GUI framework that powers the [Zed](https://github.com/zed-industries/zed) editor.

## Planned features

- **Daily journal pages** — an auto-created dated page as the default capture surface.
- **Markdown editing with rendered preview** — blocks render as formatted markdown when not focused, and switch to raw markdown on edit.
- **Outliner / block-based editing** — every line is a collapsible, nestable bullet block.
- **Bidirectional page links & backlinks** — `[[Page Name]]` syntax auto-creates pages; each page shows references back to it.
- **Block references** — `((block-id))` transcludes a specific block; edits sync everywhere it appears.
- No `#tags` — use `[[Page]]` links instead. (No plan to support Logseq's `key:: value` property syntax either.)
- **Local-first plain-markdown storage** — notes are saved as plain `.md` files on disk, syncable via Git, Dropbox, iCloud, or any other file sync.

Not currently planned (may be revisited later): graph view, query blocks, task states, flashcards, PDF annotation, whiteboards, and a plugin ecosystem.

## Installing

There are no releases yet. Once one is tagged, install or upgrade with [`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall):

```sh
cargo binstall --git https://github.com/natemcintosh/gpui-notes gpui-notes
```

`--git` is required because GPUI is a git dependency, so the crate can't be published to crates.io. Prebuilt binaries cover `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`. On any other target binstall falls back to building from source, which needs the platform setup below. The Linux binaries are built on Ubuntu 24.04 and link against its glibc, Wayland, xkbcommon and fontconfig.

Upgrading replaces only the binary. Your notes and settings are kept, because neither is stored next to the binary.

## Notes storage and settings

Pages are stored as plain `.md` files in a notes root directory. The app picks the first of these that applies:

1. the `GPUI_NOTES_ROOT` environment variable (must be an absolute path);
2. `notes_root` in `config.toml` (absolute, or starting with `~/`), read by release builds only, so a bare `cargo run` during development never touches your real graph;
3. `~/Desktop/logseq-test-zone`.

To point an installed copy at your Logseq graph, create `config.toml`:

```toml
# ~/.config/gpui-notes/config.toml   (%APPDATA%\gpui-notes\config.toml on Windows)
notes_root = "~/logseq"
```

The same directory also holds `dictionary.txt`, the words added with "Add to dictionary".

## Releasing

`just release 0.2.0` bumps the version in `Cargo.toml`, commits, tags `v0.2.0` and pushes. Pushing the tag runs `.github/workflows/release.yml`, which creates the GitHub release and uploads one `gpui-notes-<target>-v<version>.tar.gz` per target (plus a `.sha256`). Those names are what `[package.metadata.binstall]` in `Cargo.toml` expects, so change both together.

## macOS setup

Building on macOS requires the **full Xcode** plus the **Metal Toolchain** — the Command Line Tools alone are not enough. `gpui_macos` invokes `xcrun metal` at build time to compile its shaders, and that compiler ships only inside Xcode.app, with the Metal Toolchain as an on-demand component.

One-time setup:

```sh
# 1. Install Xcode from the App Store, then:
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
sudo xcodebuild -license accept
sudo xcodebuild -runFirstLaunch
xcodebuild -downloadComponent MetalToolchain
```

Verify with `xcrun metal --version` — you should see `Apple metal version ...`. If you instead see `unable to find utility "metal"` or `missing Metal Toolchain`, one of the steps above hasn't completed.

## Windows setup

Build natively with the MSVC Rust toolchain (`stable-x86_64-pc-windows-msvc`). You need the Visual Studio Build Tools ("Desktop development with C++") and a Windows 10/11 SDK: `gpui_windows` compiles its HLSL shaders with the SDK's `fxc.exe` at build time (override its location with `GPUI_FXC_PATH`). WSL is not recommended: WSLg has no hardware Vulkan driver, so GPUI falls back to slower rendering paths.

## GPUI dependency

GPUI is not published to crates.io. We depend on it (and `gpui_platform`) directly from the [`zed-industries/zed`](https://github.com/zed-industries/zed) repository. Because that repo is Zed's active development trunk, its API changes without warning — the scaffold emitted by `create-gpui-app` has already drifted from HEAD at least once (e.g. `Application::new()` was removed in favor of `gpui_platform::application()`).

To keep builds reproducible, we **pin both crates to a specific `rev`** in `Cargo.toml`. Rules of the road:

- Never depend on the floating `main` branch. Always use `rev = "..."`.
- Both `gpui` and `gpui_platform` must share the same `rev`, or Cargo will pull two copies of the Zed workspace.
- `gpui_platform` is declared per-target so each OS gets the right backend features: `runtime_shaders` + `font-kit` on macOS, `wayland` on Linux/FreeBSD, none on Windows. The `font-kit` feature is **required on macOS** — without it `gpui_macos` falls back to a `NoopTextSystem` and text shapes but never paints (cursor still renders, so it looks like a styling bug).
- To bump: pick a new commit from `zed-industries/zed`, update both `rev` values, run `cargo update -p gpui -p gpui_platform`, then `cargo check`. Expect to fix API breakage; check the relevant example in `zed/crates/gpui/examples/` for the current idiomatic usage.
- Commit the resulting `Cargo.lock` change alongside the `Cargo.toml` bump.
