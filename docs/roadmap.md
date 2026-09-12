# Roadmap

`zarr-tree` is a small, fast, read-only Rust CLI for inspecting Zarr, OME-Zarr
and SpatialData structure and metadata without reading scientific array or
chunk data. Everything on this page has to fit that sentence.

Nothing here is a promise of dates or releases. For what exists today, see
[Project status](status.md).

## Next

Small additions that answer a structural question from metadata the walk
already reads, with no new reads and no new dependency.

- The V3 `chunk_grid` name, so a non-regular grid is visible rather than just
  an unreadable `chunks` row.
- Usability fixes that come out of running the tool against real stores.

## Maybe later

Plausible, but each needs a clear use first.

- `--filter` / `--only`: show a subset of the tree. The first option that
  changes *which* nodes print rather than *what* a node says, so it needs a
  small design before any code.
- OME-Zarr `omero` channel colours, beside the labels already shown.
- OME-Zarr `image-label` metadata beyond its presence.
- OME-Zarr coordinate transformations and physical scales.
- More HCS detail: acquisitions, fields of view.
- More structural validation, within the existing model: findings over
  metadata already read, comparing a store against its own declarations.
- Faster remote walks: requests currently go out one at a time.

## Not doing

These keep the tool narrow. They are decisions, not gaps.

- **A general Zarr reader.** No chunk, pixel or array-value reads, no
  decompression, and no `zarrs` dependency to get them.
- **A data converter.** Nothing is written, repaired, migrated or rewritten.
- **A schema-validation framework.** `--validate` checks a store against its
  own declarations, never a document against a specification.
- **A policy engine.** No rule registry, configurable rules or severity
  policies.
- **A data platform.** No catalogue, cache, server, database or GUI.
- **More storage backends.** Local, S3 and HTTP(S) are the whole list.
- **Packaging ceremony.** No crates.io publishing or pre-built binaries unless
  a real need appears; `cargo install --path .` is enough.

The format-level boundaries are listed in
[Explicit non-goals](status.md#explicit-non-goals).
