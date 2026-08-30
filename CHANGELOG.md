# Changelog

Notable user-visible changes are recorded here — what a person running
`zarr-tree` would notice. Internal refactoring, test work and repository
housekeeping are left to the commit history.

The format is loosely [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses [semantic versioning](https://semver.org/spec/v2.0.0.html)
with the pre-1.0 caveat that the output format is not yet stable.

## [Unreleased]

## [0.4.0] - 2026-08-30

Metadata-only structural validation, and a documentation set to match. The
read-only, metadata-only inspection model is unchanged: nothing here reads a
chunk, an expression value or a Parquet record.

### Added

- `--validate`: a metadata-only structural check that prints findings instead
  of the tree, checking the structure a store declares against the store
  itself. It walks the store whole, and so is refused together with `--depth`.
- `PASS` / `WARN` / `ERROR` findings and a summary line. `WARN` means "could
  not be checked", never "broken" — a Parquet payload on a server that cannot
  list warns rather than erroring.
- Validation output in both forms: text, and one JSON document of `findings`
  and `summary` under `--validate --json`.
- Exit status 2 when at least one `ERROR` finding is produced, so a store can
  be checked in a shell script or CI job. Exit status 0 still means the walk
  succeeded (with `--validate`, nothing worse than a warning), and 1 still
  means the store or the command line failed.
- Zarr structural checks: every node walked into could be identified, and an
  array's `shape`, `chunks` and — when sharded — `shards` agree on how many
  dimensions there are.
- OME-Zarr checks: every `multiscales[0].datasets[].path` names a node that
  exists and is an array, and every resolution level agrees on dimensionality
  with the others and with the declared axes.
- HCS checks: every path in a plate's `wells` list names a group that exists.
- SpatialData checks: every element named in a table's `region` exists as a
  recognised image, labels, points or shapes element.
- AnnData checks: `X.shape` matches the lengths the `obs` and `var` indices
  declare. The lengths come from the index arrays' own metadata; nothing is
  counted and no value is read.
- Parquet availability checks: a points or shapes payload that is present and
  readable passes, one that is present but could not be inspected warns, and a
  payload that is genuinely absent produces no finding.

### Changed

- Minimum supported Rust version corrected from 1.85 to 1.88. Edition 2024
  alone needs only 1.85, but `object_store` 0.14 uses let-chains, which are
  stable for edition 2024 from 1.88. CI now checks the declared MSRV against
  the committed `Cargo.lock`, so the claim stays true.
- Substantially expanded documentation. The README is now a landing page, and
  the detail lives in `docs/`: getting started, a command-line reference,
  remote stores, architecture, and format references for Zarr, OME-Zarr and
  SpatialData, alongside a capability matrix and a roadmap.
- A degenerate HCS plate whose `wells` list is empty now omits the `wells`
  row rather than printing `wells: 0`. A plate with wells is unaffected.

## [0.3.0] - 2026-08-29

Remote stores, consolidated metadata, and SpatialData payload summaries. The
walk, the interpretation and both renderers stayed storage-neutral, so a remote
store produces the same tree as a local one.

### Added

- Read-only S3 access: `zarr-tree s3://bucket/path/store.zarr`. Credentials
  come from the environment — `AWS_*` variables, a web-identity token, a
  container credential endpoint, or EC2 instance metadata.
- Read-only HTTP and HTTPS access: `zarr-tree https://server/path/store.zarr`.
  Metadata is read with `GET`; children come from a WebDAV `PROPFIND`, so a
  full tree needs a WebDAV-capable server. A server that can serve but not list
  is reported as unable to list, never as missing.
- Consolidated metadata, which lets a plain static HTTP server — one that can
  never answer a `PROPFIND` — be walked in full: Zarr V2 `.zmetadata` at
  `zarr_consolidated_format` 1, and a Zarr V3 root `zarr.json` carrying an
  inline `consolidated_metadata` block. Consolidation is opportunistic (no
  store that worked without it comes to depend on it) and all-or-nothing (a
  tree is never half snapshot and half live).
- SpatialData points and shapes Parquet summaries: row count, column count,
  file count and schema, read from the file footer alone. No record, page or
  row group is decoded.
- SpatialData table summaries from AnnData written into Zarr: observations and
  variables, how `X` is represented (a dense Zarr array, or a `csr_matrix` or
  `csc_matrix` group) and its shape, the declared column counts, and the
  `region` / `region_key` / `instance_key` the table annotates. Five metadata
  reads and no listing, so it costs the same locally and remotely.
- A points element whose payload exists but could not be inspected now prints
  `parquet files: ?` and carries `"parquet": null` in `--json`, so it no longer
  reads as an element with no payload at all. A payload that is genuinely
  absent still prints nothing.

### Changed

- Remote Parquet footers are fetched with bounded suffix reads, so a payload
  summary never downloads a whole Parquet file.
- Arrays remain leaves remotely as well as locally: an array's chunk objects
  are never listed, which is what keeps a remote walk affordable.

## [0.2.0] - 2026-08-25

Zarr V3 sharding and OME-Zarr HCS. Still local-only and metadata-only.

### Added

- OME-Zarr HCS plate recognition, with a plate's declared row, column and well
  counts.
- OME-Zarr HCS well recognition.
- A `shards:` row for sharded arrays in the tree output, and a `shards` field
  in `--json` when — and only when — an array is sharded.

### Fixed

- Zarr V3 arrays using the `sharding_indexed` codec reported the shard shape as
  `chunks`. `chunks` is now the inner chunk shape and `shards` the chunk-grid
  shape, reported on separate rows.

## [0.1.0] - 2026-08-24

First release: local Zarr metadata inspection.

### Added

- Walk a local Zarr store and print its hierarchy as a tree, reading Zarr V2
  (`.zgroup` / `.zarray`) and V3 (`zarr.json`) metadata layouts.
- Label each node `[group]`, `[array]` or `[unknown]`, and show `shape`,
  `chunks` and `dtype` under every array. Arrays are leaves, so chunk objects
  are never listed.
- Recognise OME-Zarr image groups from their metadata, with the version as
  stored, the axis names from the first `multiscales` entry, and the declared
  pyramid level count and dataset paths.
- Recognise a SpatialData store root and its image, labels, points, shapes and
  table elements — from metadata markers, never from directory names.
- `--depth N` to limit traversal.
- `--json` for structured output.
- `-h` / `--help` and `-V` / `--version`.
- Quiet handling of a Unix `BrokenPipe`, so `zarr-tree store.zarr | head` exits
  0 with nothing on stderr.
- Graceful degradation throughout: an unreadable field prints `?`, an
  unreadable node prints `[unknown]`, and the rest of the walk continues.

[Unreleased]: https://github.com/ronfinn/zarr-tree/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/ronfinn/zarr-tree/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ronfinn/zarr-tree/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/ronfinn/zarr-tree/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ronfinn/zarr-tree/releases/tag/v0.1.0
