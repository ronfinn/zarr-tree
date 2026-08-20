# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project state

`zarr-tree` is an unmodified `cargo new` scaffold: a single binary crate whose `src/main.rs` still prints "Hello, world!". There are no dependencies, no tests, no modules, and no commits on `master` — every file is currently untracked.

There is no architecture to preserve yet. Treat structural decisions (binary vs. library, module layout, dependency choices) as open, and confirm direction with the user rather than inferring it from the scaffold.

## Commands

```sh
cargo check                  # type-check without linking; fastest feedback loop
cargo build                  # debug build -> target/debug/zarr-tree
cargo build --release        # optimized build -> target/release/zarr-tree
cargo run                    # build + run the binary
cargo test                   # run all tests
cargo test <name>            # run tests whose name substring-matches <name>
cargo test <name> -- --exact # run exactly one test by full path
cargo test -- --nocapture    # let tests print to stdout
cargo clippy                 # lints
cargo fmt                    # format
```

## Conventions

- Edition 2024, toolchain rustc 1.98.0. There is no `rust-toolchain.toml`, so the ambient toolchain is used.
- `Cargo.lock` is present and should stay committed — this package builds a binary.
- The user is learning Rust. Prefer explaining the idiom behind a change over applying it silently, and favor straightforward standard-library solutions over clever or heavily generic ones unless asked.

## Intent

The name suggests work with [Zarr](https://zarr.dev/) chunked N-dimensional array storage, but nothing in the repository confirms this. Do not assume Zarr-related dependencies (`zarrs`, `ndarray`, etc.) or a directory layout until the user states the goal.
