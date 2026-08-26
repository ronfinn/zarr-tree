# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`zarr-tree` is a Rust CLI for inspecting Zarr stores, in a local directory,
under an S3 key prefix, or on an HTTP server. It walks the store, reads Zarr metadata
files, and prints the hierarchy as a tree. It reads metadata only — never chunk
data.

Layout is deliberately flat: `src/main.rs` holds the whole program plus its unit
tests, and `tests/cli.rs` holds integration tests that run the compiled binary
against throwaway fixture stores.

## Current release

- Latest release: **v0.2.0** (tag `v0.2.0`).
- Current manifest version: `0.2.0`.

## Current capabilities

- Zarr V2 (`.zgroup` / `.zarray`) and V3 (`zarr.json`) metadata layouts.
- Nodes labelled `[group]`, `[array]` or `[unknown]`; arrays are leaves.
- `shape`, `chunks` and `dtype` shown under every array.
- Zarr V3 sharded arrays report `chunks` (inner chunk shape) and `shards`
  (chunk-grid shape) separately.
- OME-Zarr image detection, with the version as stored.
- Axis names from the first `multiscales` entry (both the 0.3 list-of-names and
  the 0.4/0.5 list-of-objects forms).
- Multiscale pyramid metadata: declared level count and dataset paths.
- OME-Zarr HCS plate and well recognition, with a plate's declared row,
  column and well counts.
- SpatialData store-root recognition, plus image, labels, points, shapes and
  table elements.
- Read-only S3 access: `zarr-tree s3://bucket/path/store.zarr` produces the
  same tree as a local path. Traversal, interpretation and both renderers are
  storage-neutral; only the `Store` trait and its two implementations
  (`LocalStore`, `RemoteStore`) know where the bytes come from.
- Read-only HTTP(S) access: `zarr-tree https://server/path/store.zarr`, through
  the same `RemoteStore`. Metadata is read with `GET`; children come from a
  WebDAV `PROPFIND`, so a full tree needs a WebDAV-capable server. A GET-only
  server must be reported as unable to list, never as missing — `RemoteStore`
  keeps a `reachable` flag purely to make that distinction provable.
- Arrays are leaves remotely as well, so an array's chunk objects are never
  listed. This is what makes remote traversal affordable and must stay true.
- Read-only consolidated metadata: Zarr V2 `.zmetadata` at
  `zarr_consolidated_format` 1, and a Zarr V3 root `zarr.json` carrying a
  `consolidated_metadata` block of `kind` `inline` with `must_understand`
  false. Only the forms current zarr-python writes are read. This is what lets
  a plain static HTTP server -- which can never answer a `PROPFIND` -- be
  walked in full.
- Consolidation is an overlay, not a second parser: `ConsolidatedStore` is a
  `Store` built from the document, and every semantic reader above it gets the
  same JSON it always got. It is opportunistic (no store that worked without it
  may come to depend on it) and all-or-nothing (once the document is read the
  physical store is dropped, so a tree is never half snapshot and half live).
- `--depth N` to limit traversal.
- `--json` for structured output.
- Unix `BrokenPipe` handled quietly (exit 0, nothing on stderr).

## Development rules

- Keep milestones small — one idea per commit.
- Prefer the standard library plus `serde_json`, `object_store` (the `aws` and
  `http` features only), `tokio` (a current-thread runtime, for driving
  `object_store`) and `url` (splitting an http(s) URI). That is the whole
  dependency list and it should stay that way.
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

- GCS, Azure or any object store beyond S3 and HTTP.
- Scraping HTML directory-index pages to work around a server with no WebDAV.
- Consolidation forms beyond the two above -- a V3 `kind` other than `inline`,
  or writing/refreshing a consolidated document.
- Checking consolidated metadata against the store it describes; a stale
  snapshot is reported as it stands.
- Writing to a store of any kind.
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
