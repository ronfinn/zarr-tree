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

- Latest release: **v0.3.0** (tag `v0.3.0`).
- Current manifest version: `0.3.0`.

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
- SpatialData Parquet payload summaries: row count, column count, file count
  and schema for a points or shapes element, read from the file footer only.
  A points payload is the directory `points.parquet/` of `part.N.parquet`
  parts (a listing, so it needs a backend that can list); a shapes payload is
  the single file `shapes.parquet` (no listing, so a plain static HTTP server
  can serve it). Neither path is declared in the element's metadata; both are
  SpatialData writer conventions, reached only from a group its own metadata
  already named a points or shapes element. No record, page or row group is
  ever read. A points element whose payload exists but could not be inspected
  -- the parts could not be listed, or a part's footer could not be read --
  prints `parquet files: ?` and carries `"parquet": null` in `--json`, so it
  does not read as an element with no payload. A payload that is genuinely
  absent still prints nothing.
- SpatialData table AnnData summaries, from Zarr metadata alone: observations
  and variables (each dataframe's declared `_index` array length, falling back
  to `X`'s declared shape), `X`'s representation and shape (a Zarr array is
  dense; a group of `encoding-type` `csr_matrix`/`csc_matrix` is that), the
  declared `column-order` widths, and the `region`/`region_key`/`instance_key`
  the table annotates. Five metadata reads below a table -- `obs`, `var`, each
  index array, `X` -- and no listing, so it costs the same everywhere and comes
  wholly out of a consolidated snapshot. No value, category or index label is
  read, and nothing is counted.
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
- `Store` answers five questions, not three: `read`, `children` and
  `check_root` are the Zarr walk; `files` and `read_suffix` exist for binary
  payloads alone. `read_suffix` can only ask for the end of an object, which
  is what makes "never download a whole Parquet file" structural rather than
  remembered.
- `ConsolidatedStore` keeps the physical store, but only `files` and
  `read_suffix` reach it. `read`, `children` and `check_root` stay
  overlay-only, so the Zarr snapshot is never half document and half live.
- `--depth N` to limit traversal.
- `--json` for structured output.
- `--validate`: a metadata-only structural check, printing `PASS`/`WARN`/
  `ERROR` findings instead of the tree, and a summary line. Seven rules, all
  over metadata the tree already reads: array shape/chunk/shard dimensionality,
  OME dataset paths exist and agree on dimensionality, a plate's declared well
  paths exist, a table's `region` names an existing element, an AnnData `X`
  matches the `obs`/`var` index lengths, and a points/shapes Parquet payload is
  readable. `WARN` means "could not check" and never "broken" -- a payload on a
  listing-less server warns, it does not error. Findings are a
  `Vec<ValidationFinding>`; there is no rule type, registry or engine. It walks
  the store whole and so is refused together with `--depth`. `--validate
  --json` prints one document of `findings` and `summary`.
- Exit status: 0 walked (with `--validate`, nothing worse than a warning), 1
  store or command-line failure, 2 `--validate` found at least one `ERROR`.
- Unix `BrokenPipe` handled quietly (exit 0, nothing on stderr).

## Development rules

- Keep milestones small — one idea per commit.
- Prefer the standard library plus `serde_json`, `object_store` (the `aws` and
  `http` features only), `parquet` (no default features -- no arrow, no
  codecs, no async), `tokio` (a current-thread runtime, for driving
  `object_store`) and `url` (splitting an http(s) URI). That is the whole
  dependency list and it should stay that way.
- No `zarrs` until an actual array-reading, chunk-decoding or remote-store need
  justifies it.
- The default output is metadata **inspection, not validation**. Nothing is
  checked against a specification; unfamiliar values are printed as stored.
  `--validate` is the one exception, and it checks structure a store declares
  against the store itself -- never a document against a schema.
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
- Validation rules beyond the seven above: schema or specification conformance,
  a rule registry or policy engine, per-rule or per-severity filtering, or
  `--validate` combined with `--depth`.
- Writing to a store of any kind.
- Chunk or pixel reads of any kind.
- Parquet record, page or row-group decoding; row-group, encoding, compression
  or statistics reporting; the GeoParquet `geo` block; Arrow, DataFusion or
  Polars.
- Turning an arbitrary `.parquet` file elsewhere in a store into a tree node,
  or guessing the filenames of a points payload a backend cannot list.
- AnnData beyond the table summary above: reading any value, counting non-zero
  entries, decoding categories, interpreting `uns`, `layers`, `obsm`, `obsp`,
  `varm`, `varp` or `raw`, or reading H5AD/HDF5.
- async / Tokio.

## Conventions

- Edition 2024, MSRV 1.85 (`rust-version` in `Cargo.toml`). No
  `rust-toolchain.toml`, so the ambient toolchain is used; developed on 1.98.0.
- `Cargo.lock` is committed — this package builds a binary.
- The user is learning Rust. Explain the idiom behind a change rather than
  applying it silently, and favour straightforward standard-library solutions
  over clever or heavily generic ones unless asked.
