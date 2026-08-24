# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`zarr-tree` is a Rust CLI for inspecting local Zarr stores. It walks a directory,
reads Zarr metadata files, and prints the hierarchy as a tree. It reads metadata
only — never chunk data.

Layout is deliberately flat: `src/main.rs` holds the whole program plus its unit
tests, and `tests/cli.rs` holds integration tests that run the compiled binary
against throwaway fixture stores.

## Current release

- Latest release: **v0.1.0** (tag `v0.1.0`).
- Current manifest version: `0.1.0`.

## Current capabilities

- Zarr V2 (`.zgroup` / `.zarray`) and V3 (`zarr.json`) metadata layouts.
- Nodes labelled `[group]`, `[array]` or `[unknown]`; arrays are leaves.
- `shape`, `chunks` and `dtype` shown under every array.
- OME-Zarr image detection, with the version as stored.
- Axis names from the first `multiscales` entry (both the 0.3 list-of-names and
  the 0.4/0.5 list-of-objects forms).
- Multiscale pyramid metadata: declared level count and dataset paths.
- SpatialData store-root recognition, plus image, labels, points, shapes and
  table elements.
- `--depth N` to limit traversal.
- `--json` for structured output.
- Unix `BrokenPipe` handled quietly (exit 0, nothing on stderr).

## Development rules

- Keep milestones small — one idea per commit.
- Prefer the standard library plus `serde_json`. That is the whole dependency
  list and it should stay that way.
- No `zarrs` until an actual array-reading, chunk-decoding or remote-store need
  justifies it.
- This is metadata **inspection, not validation**. Nothing is checked against a
  specification; unfamiliar values are printed as stored.
- Never infer scientific semantics from directory names. A group is recognised
  from its metadata markers or not at all.
- Malformed metadata degrades gracefully where support already exists: an
  unreadable field prints `?`, an unreadable node prints `[unknown]`, and the
  rest of the walk continues.

## Quality gate

Run all of these before committing:

```sh
cargo fmt
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
```

Useful variants: `cargo test <name>` to substring-match, `cargo test <name> -- --exact`
for exactly one, `cargo test -- --nocapture` to see printed output.

## Git

- Post-v0.1.0 work happens on branches, not directly on `master`.
- Commit cohesive changes — one logical change per commit.
- Never force-push `master`.
- Release tags are immutable; never move or re-cut one.

## Known out-of-scope areas

Do not implement these without an explicit request:

- S3 / HTTP / any remote or object store.
- Chunk or pixel reads of any kind.
- Parquet decoding.
- AnnData interpretation.
- async / Tokio.

## Conventions

- Edition 2024, MSRV 1.85 (`rust-version` in `Cargo.toml`). No
  `rust-toolchain.toml`, so the ambient toolchain is used; developed on 1.98.0.
- `Cargo.lock` is committed — this package builds a binary.
- The user is learning Rust. Explain the idiom behind a change rather than
  applying it silently, and favour straightforward standard-library solutions
  over clever or heavily generic ones unless asked.
