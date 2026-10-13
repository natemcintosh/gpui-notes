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
