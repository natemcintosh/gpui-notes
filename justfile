[private]
default:
    @just --list

alias b := build
alias t := test
alias c := check

build:
    cargo build --release

test:
    cargo nextest run --no-fail-fast

check:
    cargo clippy -- -W clippy::pedantic
