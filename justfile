[private]
default:
    @just --list

alias b := build
alias r := run
alias t := test
alias c := check
alias u := unblocked

build:
    cargo build --release

# Refresh ~/Desktop/logseq-test-zone from ~/logseq, then run against it
run: test-zone
    GPUI_NOTES_ROOT="$HOME/Desktop/logseq-test-zone" cargo run --release

# Run against the real Logseq graph (~/logseq, or a given absolute path) — edits are saved to your actual notes
real $GPUI_NOTES_ROOT=(home_directory() / "logseq"):
    cargo run --release

test:
    cargo nextest run --no-fail-fast

check:
    cargo clippy --all-targets -- -D warnings -W clippy::pedantic

pre:
    prek run --all-files

unblocked:
    nu ./scripts/unblocked-issues.nu

# Reset ~/Desktop/logseq-test-zone to a fresh copy of the real Logseq graph (~/logseq)
test-zone:
    rsync -a --delete --exclude .stfolder ~/logseq/ "$HOME/Desktop/logseq-test-zone/"

# Cut a release: bump Cargo.toml to <version>, commit, tag v<version> and push; CI then builds and uploads binaries
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "$(git status --porcelain)" ]; then echo "working tree not clean" >&2; exit 1; fi
    # First `version =` line only: that's [package]'s.
    perl -0pi -e 's/^version = ".*?"$/version = "{{version}}"/m' Cargo.toml
    grep -q '^version = "{{version}}"$' Cargo.toml || { echo "couldn't set version in Cargo.toml" >&2; exit 1; }
    cargo check --quiet
    git commit -am "chore(release): v{{version}}"
    git tag -a "v{{version}}" -m "v{{version}}"
    git push origin main "v{{version}}"
