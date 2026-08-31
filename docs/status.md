# Project status

| | |
| --- | --- |
| Latest release | [v0.4.0](https://github.com/ronfinn/zarr-tree/releases/tag/v0.4.0) |
| Development status | at v0.4.0, on `master` |
| Minimum supported Rust version | 1.88 |
| Tests | 94 unit, 18 integration |
| License | MIT |

This document records what `zarr-tree` implements today. It is a capability
matrix, not a specification-conformance claim: `zarr-tree` reads metadata and
prints what it finds, and unfamiliar values are shown as stored.

Two states appear in the tables below:

| Status | Meaning |
| --- | --- |
| Supported | Implemented and released, as of v0.4.0. |
| Not implemented | Absent, and listed here because it is a reasonable thing to expect. |

## Storage

| Capability | Status |
| --- | --- |
| Local filesystem | Supported |
| Amazon S3 (`s3://`) | Supported |
| HTTP/HTTPS metadata reads (`GET`) | Supported |
| WebDAV hierarchy listing (`PROPFIND`) | Supported |
| Static HTTP via consolidated metadata | Supported |
| Google Cloud Storage | Not implemented |
| Azure Blob Storage | Not implemented |
| ZIP stores | Not implemented |
| Writing to a store | Not implemented, and out of scope |

S3 credentials come from the environment — `AWS_*` variables, a web-identity
token, a container credential endpoint or EC2 instance metadata. `~/.aws/
credentials` is not read. HTTP access is anonymous.

## Zarr

| Capability | Status |
| --- | --- |
| Zarr V2 (`.zgroup`, `.zarray`) | Supported |
| Zarr V3 (`zarr.json`) | Supported |
| Array `shape`, `chunks`, `dtype` | Supported |
| V3 sharding (`sharding_indexed`), chunks and shards reported separately | Supported |
| Arrays treated as leaves — chunk objects never listed | Supported |
| V2 consolidated metadata (`.zmetadata`, format 1) | Supported |
| V3 inline `consolidated_metadata` (`kind: inline`, `must_understand: false`) | Supported |
| Other consolidation forms | Not implemented |
| Checking consolidated metadata against the store it describes | Not implemented |
| Compressors, fill values, user attributes | Not implemented |
| V3 dtypes given in object (extension) form, reported by name | Supported |
| V3 `dimension_names`, shown in order with unnamed dimensions kept in place | Supported |
| Natural ordering of child names, so `2` sorts before `10` | Supported |

V2 `dtype` strings are printed exactly as stored, in NumPy notation, and are not
translated into V3 names. A V3 extension dtype shows the `name` its object
declares; the `configuration` beside it is not displayed or interpreted, and a
`data_type` with no usable name shows as `?`. A directory carrying both V2 and V3 metadata is
reported as V2. The [Zarr reference](zarr.md) documents each of these in
detail.

## OME-Zarr

| Capability | Status |
| --- | --- |
| OME-Zarr 0.3 (`axes` as a list of names) | Supported |
| OME-Zarr 0.4 (`axes` as a list of objects, V2 layout) | Supported |
| OME-Zarr 0.5 (`attributes.ome` in `zarr.json`) | Supported |
| Version shown as stored, including unfamiliar values | Supported |
| Axis names from the first `multiscales` entry | Supported |
| Declared pyramid level count | Supported |
| Multiscale dataset paths | Supported |
| HCS plates, with declared row, column and well counts | Supported |
| HCS wells | Supported |
| `image-label` presence, to tell a segmentation from an image | Supported |
| Axis `type` and `unit` | Not implemented |
| Coordinate transformations, scale factors, physical extents | Not implemented |
| `omero` / channel metadata | Not implemented |
| Acquisitions, field-of-view indices | Not implemented |
| Full OME-NGFF specification conformance checking | Not implemented, and out of scope |

The declared pyramid level count comes from `multiscales[0].datasets`, never
from counting child directories. Group kinds are recognised from metadata
markers alone; nothing is inferred from a directory name. The
[OME-Zarr reference](ome-zarr.md) documents each of these in detail, including
exactly what `image-label` does and does not do.

## SpatialData

| Capability | Status |
| --- | --- |
| Store root recognition, with container format version | Supported |
| Image elements | Supported |
| Labels elements | Supported |
| Points elements | Supported |
| Shapes elements | Supported |
| Table elements | Supported |
| Points/shapes Parquet summary: rows, columns, file count, schema | Supported |
| AnnData table summary: observations, variables, `X`, column counts, annotated region | Supported |
| Parquet record, page or row-group decoding | Not implemented, and out of scope |
| Row-group layout, encodings, compression, statistics, GeoParquet `geo` | Not implemented |
| Expression or annotation values, categories, index labels | Not implemented |
| `layers`, `obsm`, `obsp`, `varm`, `varp`, `uns`, `raw` interpretation | Not implemented |
| H5AD / HDF5 | Not implemented, and out of scope |
| Element axes, feature keys, geometry types, coordinate transformations | Not implemented |

A Parquet payload is read from its footer alone, at the two paths SpatialData's
writer uses: `points.parquet/` for a points element and `shapes.parquet` for a
shapes element. An arbitrary `.parquet` file elsewhere in a store is not read.

A table summary is AnnData *metadata*: five metadata reads and no listing, so it
costs the same locally and remotely and comes wholly out of a consolidated
snapshot. `X` is described when written as a Zarr array, a `csr_matrix` or a
`csc_matrix`; any other representation draws no `X` row.

The [SpatialData reference](spatialdata.md) documents all of this in detail.

## Validation

`--validate` is a metadata-only structural check, released in v0.4.0.

| Capability | Status |
| --- | --- |
| `--validate` | Supported |
| `PASS` / `WARN` / `ERROR` findings and a summary line | Supported |
| Text output | Supported |
| JSON output (`--validate --json`) | Supported |
| Exit status 2 when at least one `ERROR` is found | Supported |
| Array shape / chunk / shard / dimension-name dimensionality agreement | Supported |
| OME dataset paths exist and agree on dimensionality | Supported |
| Declared HCS well paths exist | Supported |
| A table's `region` names an existing element | Supported |
| AnnData `X` matches the `obs` / `var` index lengths | Supported |
| Points/shapes Parquet payload is readable | Supported |
| Combining `--validate` with `--depth` | Refused by design |
| Combining `--validate` with `--attributes` | Refused by design |
| Schema or specification conformance checking | Not implemented, and out of scope |
| A rule registry, policy engine, or per-rule/per-severity filtering | Not implemented, and out of scope |

`WARN` means "could not be checked", never "broken": a points payload on a
listing-less server warns rather than erroring. Seven rules exist; there is no
mechanism for adding an eighth from outside the source.

## Command line

| Capability | Status |
| --- | --- |
| `--depth N` | Supported |
| `--json` | Supported |
| `--validate` | Supported |
| `--attributes` | Supported |
| `-h` / `--help`, `-V` / `--version` | Supported |
| Exit status 0 walked, 1 failure, 2 validation error | Supported (2 on master) |
| Quiet `BrokenPipe` handling for `\| head`, `\| less` | Supported |
| Filtering, colour, or any other output option | Not implemented |

Every option is documented in full in the
[Command-line reference](cli.md).

## Explicit non-goals

These are not gaps waiting to be filled. They are boundaries the design depends
on:

- **Chunk and pixel reads of any kind.** Arrays are leaves; an array's chunk
  objects are never listed, which is what makes a remote walk affordable.
- **Expression matrix reads.** A table summary counts nothing and opens no
  array.
- **Parquet record reads.** Only the footer is fetched, so row counts and
  schemas are what a file *declares*.
- **H5AD / HDF5.** Only AnnData written into Zarr is read.
- **Complete OME-NGFF validation.** `--validate` checks a store against its own
  declarations, never a document against a schema.
- **Repairing, editing or writing a store.** `zarr-tree` opens nothing for
  writing.
- **Scraping HTML directory-index pages** to work around a server with no
  WebDAV and no consolidated metadata.

## See also

- [Getting started](getting-started.md) — building it, and a first store.
- [Command-line reference](cli.md) — every option, the JSON fields, the
  validation rules, exit statuses, and shell and CI patterns.
- [Remote stores](remote-stores.md) — S3, HTTP, WebDAV and static HTTP.
- [Architecture](architecture.md) — how the storage, classification and
  validation layers fit together.
- [Zarr reference](zarr.md) — V2 and V3 layouts, arrays, sharding, consolidated
  metadata, and the degradation model.
- [OME-Zarr reference](ome-zarr.md) — recognition, versions, axes, multiscale
  datasets, plates and wells.
- [SpatialData reference](spatialdata.md) — store and element recognition, the
  Parquet and AnnData payload summaries, and region linkage.
- [Roadmap](roadmap.md) — direction, with nothing promised.
- [Changelog](../CHANGELOG.md) — which release each capability above arrived
  in.
- [Contributing](../CONTRIBUTING.md) — building it, the quality gate, and the
  design constraints behind the non-goals above.
- [README](../README.md) — the project overview.
