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

run:
    cargo run --release

test:
    cargo nextest run --no-fail-fast

check:
    cargo clippy --all-targets -- -D warnings

pre:
    prek run --all-files

unblocked:
    ./scripts/unblocked-issues.sh
