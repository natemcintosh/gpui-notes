[private]
default:
    @just --list

alias b := build
alias r := run
alias t := test
alias c := check

build:
    cargo build --release

run:
    cargo run --release

test:
    cargo nextest run --no-fail-fast

check:
    cargo clippy -- -W clippy::pedantic

pre:
    prek run --all-files

unblocked:
    ./scripts/unblocked-issues.sh
