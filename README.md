# zarr-tree

[![CI](https://github.com/ronfinn/zarr-tree/actions/workflows/ci.yml/badge.svg)](https://github.com/ronfinn/zarr-tree/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/ronfinn/zarr-tree?label=release)](https://github.com/ronfinn/zarr-tree/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange)](#project-status)

`zarr-tree` is a read-only Rust CLI for inspecting the hierarchy and metadata of
Zarr stores — on a local filesystem, in an S3 bucket, or on an HTTP server. It
reads Zarr V2 and V3 layouts, recognises OME-Zarr and SpatialData conventions
from their metadata markers, and can check the structure a store declares
against the store itself.

It reads metadata only. Arrays are leaves: chunk objects are never listed and
chunk data is never fetched.

```
$ zarr-tree --depth 2 experiment.zarr
experiment.zarr [group, SpatialData 0.2]
├─ zarr: V3
├── images [group]
│   ├─ zarr: V3
│   └── morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]
│       ├─ zarr: V3
│       ├─ axes: c, y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── labels [group]
│   ├─ zarr: V3
│   └── nuclei [group, OME-Zarr 0.5-dev-spatialdata, SpatialData labels]
│       ├─ zarr: V3
│       ├─ axes: y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── points [group]
│   ├─ zarr: V3
│   └── transcripts [group, SpatialData points]
│       └─ zarr: V3
├── shapes [group]
│   ├─ zarr: V3
│   └── cell_boundaries [group, SpatialData shapes]
│       └─ zarr: V3
└── tables [group]
    ├─ zarr: V3
    └── table [group, SpatialData table]
        ├─ zarr: V3
        ├─ observations: 1,200
        ├─ variables: 313
        ├─ X: dense [1200, 313] float32
        ├─ obs columns: 3
        ├─ var columns: 2
        ├─ annotates: cell_boundaries
        ├─ region key: region
        └─ instance key: cell_id
```

## Why zarr-tree?

A Zarr store is not a file. It is a directory tree, or an object-store key
prefix, whose structure lives in small JSON documents scattered through it —
and whose bulk is chunk objects, often millions of them. `ls`, `tree` and
`aws s3 ls` show the bulk and bury the structure.

What you usually want to know is the structure: what groups are in here, what
arrays, what shape and dtype, and — for a microscopy or spatial-omics store —
what the OME-Zarr and SpatialData metadata says the pieces are and how they
relate. That information is spread across several conventions layered on top of
plain Zarr, and reading it by hand means opening a dozen JSON files.

`zarr-tree` reads those documents and prints one tree. It stops at array and
chunk boundaries deliberately: an array is a leaf, so a walk costs a few
requests per node and none at all per chunk — which is what makes a store you
could not afford to list at all inspectable. Nothing is inferred from a
directory name, and no scientific data is read.

## Quick start

Build from source with a Rust toolchain of 1.88 or newer:

```sh
git clone https://github.com/ronfinn/zarr-tree.git
cd zarr-tree
cargo install --path .
```

Then point it at a store. A local directory, an S3 URI and an HTTP URL take the
same walk, print the same tree and accept the same options:

```sh
zarr-tree /data/example.zarr
zarr-tree s3://bucket/path/example.zarr
zarr-tree https://example.org/path/example.zarr
```

Limit how far it descends, or print the same walk as JSON:

```sh
zarr-tree --depth 2 example.zarr
zarr-tree --json example.zarr | jq '.children[].name'
```

Check what the metadata declares against what the store has:

```sh
zarr-tree --validate example.zarr
```

[docs/getting-started.md](docs/getting-started.md) covers this at more length,
[docs/cli.md](docs/cli.md) is the full command-line reference, and
[docs/remote-stores.md](docs/remote-stores.md) covers the S3 and HTTP cases.

## What it understands

| Area | Support |
| --- | --- |
| Zarr | V2 and V3, sharding, consolidated metadata |
| OME-Zarr | 0.3–0.5, multiscales, axes, dataset paths, HCS plates and wells |
| SpatialData | store root, images, labels, points, shapes, tables |
| Parquet | footer summaries only — rows, columns, file count, schema |
| AnnData | metadata summaries only — no value is read, nothing is counted |
| Storage | local filesystem, S3, HTTP/WebDAV, static HTTP via consolidated metadata |
| Validation | metadata-only structural checks |

[docs/status.md](docs/status.md) carries the exact matrix, including what is
deliberately not implemented.

## Project status

The latest release is [v0.4.0](https://github.com/ronfinn/zarr-tree/releases),
which added metadata-only structural validation (`--validate`).

| | |
| --- | --- |
| Latest release | v0.4.0 |
| Tests | 154 passing — 128 unit, 26 integration |
| Minimum supported Rust version | 1.88 |

This is a small utility maintained by one person, not a certified product. The
scope is deliberately narrow and the [non-goals](docs/status.md#explicit-non-goals)
are as much a part of the design as the features.

## Documentation

- [Getting started](docs/getting-started.md) — build it and inspect a first
  store, as a tutorial.
- [Command-line reference](docs/cli.md) — every option, the JSON fields, the
  seven validation rules, exit statuses, and shell and CI patterns.
- [Remote stores](docs/remote-stores.md) — S3, AWS credentials and endpoints,
  HTTP and WebDAV, static HTTP via consolidated metadata, troubleshooting.
- [Architecture](docs/architecture.md) — the `Store` trait, the consolidated
  overlay, how metadata is classified, and how validation reuses the walk.
- [Zarr reference](docs/zarr.md) — V2 and V3 layouts, arrays and dtypes,
  sharding, consolidated metadata, and how malformed metadata degrades.
- [OME-Zarr reference](docs/ome-zarr.md) — recognition, versions, axes,
  multiscale datasets, HCS plates and wells, and what is deliberately absent.
- [SpatialData reference](docs/spatialdata.md) — store and element recognition,
  the Parquet and AnnData payload conventions, region linkage, and the
  SpatialData validation rules.
- [Project status](docs/status.md) — the capability matrix, and what is
  deliberately absent.
- [Roadmap](docs/roadmap.md) — direction, with nothing promised.
- [Releases](https://github.com/ronfinn/zarr-tree/releases)

Contributing and maintenance:

- [Changelog](CHANGELOG.md) — user-visible changes, per release.
- [Contributing](CONTRIBUTING.md) — setup, the quality gate, and the design
  constraints a change is expected to preserve.
- [Security policy](SECURITY.md) — scope, and how to report a vulnerability.

The storage-backend and format sections below are being split out of this file
one subject at a time.

## Features

- Prints a tree of a Zarr store from a directory, an `s3://` URI or an
  `http(s)://` URL — the same walk, the same output and the same options
  wherever the bytes are.
- Reads Zarr V2 and V3 metadata layouts, labelling each node `[group]`,
  `[array]` or `[unknown]`, and showing `shape`, `chunks` and `dtype` under
  every array.
- Says which metadata format each node was read as, on a `zarr: V2`/`zarr: V3`
  row and as `zarr_format` in `--json`. A store need not be all one version,
  and this reports the reading actually taken — see
  [docs/zarr.md](docs/zarr.md#format-version).
- Tells a Zarr V3 sharded array's inner chunks from its shards and reports both
  — see [docs/zarr.md](docs/zarr.md#sharding).
- Stops descending at arrays, so an array's chunk objects are never listed,
  however many millions of them there are. That is what makes a remote walk
  affordable.
- Recognises OME-Zarr images, HCS plates and wells from their metadata markers,
  and shows an image's axis names, declared pyramid level count and dataset
  paths — see [docs/ome-zarr.md](docs/ome-zarr.md).
- Recognises a SpatialData store root and its image, labels, points, shapes and
  table elements, from metadata markers rather than directory names.
- Summarises a points or shapes element's Parquet payload — rows, columns, file
  count and schema — from the file footer alone. **No Parquet record is read.**
- Summarises the AnnData table inside a table element — observations,
  variables, how `X` is stored, column counts, and what it annotates — from
  Zarr metadata alone. **No expression or annotation value is read.** See
  [docs/spatialdata.md](docs/spatialdata.md).
- Reads a store's consolidated metadata when it has any, and walks the whole
  tree from that one document — so a plain static HTTP server, which can never
  answer a listing, needs none.
- Checks the structure a store declares against the store itself with
  `--validate`: `PASS`/`WARN`/`ERROR` a line, and exit status 2 when something
  declared is missing.
- Limits depth with `--depth N`, prints the same walk as JSON with `--json`,
  and sits quietly at the producing end of a pipe.
- Degrades gracefully: metadata that cannot be read or parsed costs only that
  node's label or a single field, never the rest of the walk.

## Zarr

Both Zarr metadata layouts are read, and only the fields needed to identify a
node and describe its structure:

| Concept | V2 | V3 |
| --- | --- | --- |
| Group marker | `.zgroup` | `zarr.json`, `"node_type": "group"` |
| Array marker | `.zarray` | `zarr.json`, `"node_type": "array"` |
| Attributes | `.zattrs` | `attributes` in `zarr.json` |
| Chunk shape | `chunks` | `chunk_grid`, or the sharding codec when sharded |
| dtype | `dtype`, NumPy notation | `data_type` |
| Codec chain | `filters`, then `compressor`, by `id` | `codecs`, by `name` |
| Consolidation | `.zmetadata` | inline `consolidated_metadata` |

Every recognised node says which of the two it was read as, on a `zarr:` row of
its own. V2 is checked first, so a directory carrying both is reported as V2 —
in the row as everywhere else, because V2 is the document the node's other
fields came out of. V2 dtypes
are printed exactly as stored — `<u2`, `|u1` — and are never translated. A
sharded V3 array reports `chunks` (the inner chunk shape) and `shards` (the
chunk grid) under separate names:

```
$ zarr-tree sharded.zarr
sharded.zarr [group]
├─ zarr: V3
└── img [array]
    ├─ zarr:   V3
    ├─ shape:  [4096, 4096]
    ├─ chunks: [512, 512]
    ├─ shards: [2048, 2048]
    └─ dtype:  uint16
```

Arrays are leaves: once a node is an array the walk stops, so chunk storage —
a V3 `c/` tree, V2 chunk keys — is never listed at any depth.

**[docs/zarr.md](docs/zarr.md)** is the reference: the exact fields read from
each layout, sharding semantics, consolidated metadata, how malformed metadata
degrades, and what is deliberately not implemented.

## OME-Zarr

An [OME-Zarr](https://ngff.openmicroscopy.org/) image is an ordinary Zarr group
whose attributes carry a `multiscales` key. Groups like that are tagged, with
the version as stored, and their axis names, declared pyramid level count and
dataset paths are shown:

```
$ zarr-tree image.zarr
image.zarr [group, OME-Zarr 0.4]
├─ zarr: V2
├─ axes: c, y, x
├─ pyramid levels: 3
├─ datasets: 0, 1, 2
├── 0 [array]
│   ├─ zarr:   V2
│   ├─ shape:  [2, 2048, 2048]
│   ├─ chunks: [1, 512, 512]
│   └─ dtype:  <u2
...
```

HCS plates and wells are recognised the same way — from a `plate` or `well` key
in the attributes, never from a directory name — and a plate shows its declared
row, column and well counts. Every count comes from the metadata and never from
counting directories.

The tree is **recognition, not validation**: nothing it prints is checked
against the OME-NGFF specification. `--validate` adds the structural checks
listed in [docs/cli.md](docs/cli.md#the-seven-rules); for conformance checking use the
[OME-NGFF validator](https://ome.github.io/ome-ngff-validator/).

**[docs/ome-zarr.md](docs/ome-zarr.md)** is the reference: recognition rules,
version and metadata locations, both axis forms, multiscale datasets, plates
and wells, what `image-label` does and does not do, and the current
limitations.

## SpatialData

[SpatialData](https://spatialdata.scverse.org/) keeps a spatial omics
experiment in one Zarr container: microscopy images, segmentation masks,
transcript locations, geometries and annotation tables. `zarr-tree` recognises
a store root and its five element kinds from their metadata markers, and
summarises the two payload formats those elements use:

```
$ zarr-tree --depth 2 experiment.zarr
experiment.zarr [group, SpatialData 0.2]
├─ zarr: V3
├── images [group]
│   ├─ zarr: V3
│   └── morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]
│       ├─ zarr: V3
│       ├─ axes: c, y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── labels [group]
│   ├─ zarr: V3
│   └── nuclei [group, OME-Zarr 0.5-dev-spatialdata, SpatialData labels]
│       ├─ zarr: V3
│       ├─ axes: y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── points [group]
│   ├─ zarr: V3
│   └── transcripts [group, SpatialData points]
│       ├─ zarr: V3
│       ├─ rows: 3,714
│       ├─ columns: 4
│       ├─ parquet files: 2
│       └─ schema: x:double, y:double, feature_name:string, cell_id:int32
├── shapes [group]
│   ├─ zarr: V3
│   └── cell_boundaries [group, SpatialData shapes]
│       ├─ zarr: V3
│       ├─ rows: 1,200
│       ├─ columns: 2
│       ├─ parquet files: 1
│       └─ schema: geometry:byte_array, cell_id:int32
└── tables [group]
    ├─ zarr: V3
    └── table [group, SpatialData table]
        ├─ zarr: V3
        ├─ observations: 1,200
        ├─ variables: 313
        ├─ X: dense [1200, 313] float32
        ├─ obs columns: 3
        ├─ var columns: 2
        ├─ annotates: cell_boundaries
        ├─ region key: region
        └─ instance key: cell_id
```

| Element | Recognised from | Summarised from |
| --- | --- | --- |
| root | `spatialdata_attrs.spatialdata_software_version` | — |
| image | `spatialdata_attrs` and OME-Zarr `multiscales` | Zarr and OME-Zarr metadata |
| labels | the same, plus OME-Zarr `image-label` | Zarr and OME-Zarr metadata |
| points | `encoding-type` = `ngff:points` | Parquet footers |
| shapes | `encoding-type` = `ngff:shapes` | a Parquet footer |
| table | `spatialdata-encoding-type` = `ngff:regions_table` | AnnData's Zarr metadata |

Every one of those markers is matched exactly, and **nothing is inferred from a
directory name**: the `images`, `points` and `tables` container groups carry no
attributes at all, so they are printed untagged.

A points or shapes element keeps its data outside the Zarr hierarchy, in
Parquet. Only the **file footer** is read — 64 KiB from the end of each file,
at the two paths SpatialData's writer uses — so no record, page or row group is
decoded, and a points element's `points.parquet/` directory is data rather than
a child node and is not drawn as one. A table holds an
[AnnData](https://anndata.readthedocs.io/) object, and its summary is **Zarr
metadata alone**: five reads and no listing, with no expression value,
annotation value or category read and nothing counted.

**[docs/spatialdata.md](docs/spatialdata.md)** is the reference: root and
element recognition, the Parquet and AnnData payload conventions, footer
access, readable-versus-unavailable-versus-absent payloads, region linkage, the
JSON shape, the three SpatialData validation rules, and the current
limitations.

## Installation

`zarr-tree` is not on crates.io and there are no release binaries yet, so it is
built from source:

```sh
git clone https://github.com/ronfinn/zarr-tree.git
cd zarr-tree
cargo build --release      # binary at target/release/zarr-tree
cargo install --path .     # or install it into ~/.cargo/bin
```

Rust 1.88 or newer is required. Edition 2024 itself needs only 1.85, but the
current `object_store` release uses let-chains, which are stable for edition
2024 from 1.88. It was developed with rustc 1.98.0.

[docs/getting-started.md](docs/getting-started.md) walks through the build, a
first store and the option set in more detail.

## Usage

```sh
zarr-tree [OPTIONS] <STORE>
```

The store is a directory, an `s3://` URI or an `http(s)://` URL, and the walk,
the tree and the options are the same for all three:

```sh
zarr-tree /data/example.zarr
zarr-tree s3://bucket/example.zarr
zarr-tree https://example.org/example.zarr
```

| Option | Meaning |
| --- | --- |
| `--depth <N>` | Descend at most N levels below the root. Arrays are leaves at any depth. |
| `--json` | Print the same walk as one JSON document. Combines with `--depth` and `--validate`. |
| `--validate` | Check the structure the metadata declares against the store, and print findings instead of the tree. |
| `--attributes` | Show each node's user attributes as stored, uninterpreted. Combines with `--depth` and `--json`; not with `--validate`. |
| `-h`, `--help` | Print help. |
| `-V`, `--version` | Print version. |

| Exit status | Meaning |
| --- | --- |
| 0 | The store was walked. With `--validate`, nothing worse than a `WARN`. |
| 1 | The store could not be read, or the command line made no sense. |
| 2 | `--validate` ran and reported at least one `ERROR`. |

`zarr-tree` sits quietly at the producing end of a pipe:
`zarr-tree big.zarr | head` stops when the reader does, with nothing on stderr
and exit status 0.

**[docs/cli.md](docs/cli.md) is the complete command-line reference** — every
option in detail, the JSON field tables and their absence-versus-`null`
semantics, the seven validation rules and their severities, the exit-status
contract, `jq` recipes, and shell and CI patterns.

The three storage backends are covered below, and in full in
[docs/remote-stores.md](docs/remote-stores.md).

### S3

A store argument beginning with `s3://` is read from S3 instead of from disk.
Nothing else changes: the same walk, the same tree, the same `--depth` and
`--json`.

```
$ zarr-tree --depth 1 s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr
s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr [group]
├─ zarr: V2
├── mapping [group]
│   └─ zarr: V2
├── recon-1 [group]
│   └─ zarr: V2
└── recon-2 [group]
    └─ zarr: V2
```

The scheme is the only thing that decides. Every other argument is a path on
this machine, exactly as before — including a relative path that happens to
contain `s3://` somewhere after its start.

**Credentials.** Settings come from the usual `AWS_*` environment variables and
nothing else — there is no login, no profile manager and no credential file of
zarr-tree's own. When none of them names a credential, requests go out
**unsigned**, which is what a public bucket wants and what the example above
relies on; `AWS_SKIP_SIGNATURE=false` forces the credential chain instead.
`AWS_REGION` matters, because without it the bucket is assumed to be in
`us-east-1`. `~/.aws/credentials` is not read, so a named profile has no effect
on its own. [docs/remote-stores.md](docs/remote-stores.md#aws-credentials) has
the full variable list, the anonymous-default rationale and the profile bridge.

**Requests.** An array is a leaf on S3 exactly as it is on disk, and that is
what makes remote traversal affordable: a listing is made only for a group, so
an array's chunk objects are never enumerated. Walking a six-level pyramid
whose first two levels alone hold some 77,000 chunk objects costs 2 listings
and 16 metadata reads:

```
$ zarr-tree s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr/recon-1/em
s3://.../recon-1/em [group]
├─ zarr: V2
└── fibsem-uint16 [group, OME-Zarr 0.4]
    ├─ zarr: V2
    ├─ axes: z, y, x
    ├─ pyramid levels: 6
    ├─ datasets: s0, s1, s2, s3, s4, s5
    ├── s0 [array]
    │   ├─ zarr:   V2
    │   ├─ shape:  [12664, 1200, 8750]
    │   ├─ chunks: [256, 256, 256]
    │   └─ dtype:  <u2
    ...
```

Per node that is one `ListObjectsV2` with `delimiter=/` for a group, and one to
three `GetObject` calls to classify it — `.zgroup`, then `.zarray`, then
`zarr.json`, stopping at the first that answers.

**Errors** name the category and nothing more:

```
$ zarr-tree s3://janelia-cosem-datasets/nope.zarr
error: no such bucket or prefix: s3://janelia-cosem-datasets/nope.zarr

$ zarr-tree s3://
error: expected s3://bucket/prefix, not "s3://"
usage: zarr-tree [OPTIONS] <STORE>
```

### HTTP and HTTPS

A store argument beginning with `http://` or `https://` is read from that
server. Again nothing else changes: the same walk, the same tree, the same
`--depth` and `--json`.

```
$ zarr-tree http://server.example/data/example.zarr
http://server.example/data/example.zarr [group, OME-Zarr 0.5]
├─ zarr: V3
├─ axes: y, x
├─ pyramid levels: 1
├─ datasets: 0
├── 0 [array]
│   ├─ zarr:   V3
│   ├─ shape:  [1024, 1024]
│   ├─ chunks: [128, 128]
│   ├─ shards: [512, 512]
│   └─ dtype:  uint16
└── labels [group]
    ├─ zarr: V3
    └── cells [array]
        ├─ zarr:   V3
        ├─ shape:  [1024, 1024]
        ├─ chunks: [256, 256]
        └─ dtype:  uint8
```

**Listing needs WebDAV.** Metadata is read with ordinary `GET` requests, which
every server supports. Finding a node's *children* is a different question, and
HTTP has no operation for it — so `zarr-tree` asks with a WebDAV `PROPFIND`,
`Depth: 1`. A server configured for WebDAV gives a full tree; an ordinary
static file server does not, and is told apart from a missing store, because
saying "not found" about a URL we have just read metadata from would be wrong:

```
$ zarr-tree --depth 1 https://static.example/data/example.zarr
https://static.example/data/example.zarr [group, OME-Zarr 0.4]
error: cannot list https://static.example/data/example.zarr: the server answers
GET but not the WebDAV listing needed to find child nodes
```

Such a store can still be inspected one node at a time with `--depth 0`, which
needs no listing at all — and in full at any depth if it carries [consolidated
metadata](#consolidated-metadata), which is what lets a plain static server
serve a whole tree. Directory-index pages are never scraped: an HTML listing is
a page for people, not a protocol, and reading one would mean guessing at a
server's theme.

**URLs.** The URL is the store root, and internal paths resolve beneath it. A
query string is kept and sent with every request, which is the one shape of
access token a static server tends to want. There is no credential handling of
any other kind: no `Authorization` header, no cookie, no `--user`.

[docs/remote-stores.md](docs/remote-stores.md) is the practical guide to all of
this: which servers list, what a static store can and cannot do, how remote
Parquet footers are read, and what to check when a remote store will not open.

### Consolidated metadata

Walking a store means reading one small metadata file per node and asking, at
every group, what lies beneath it. On a directory that is cheap. On S3 it is a
request per node and a listing per group; on a static HTTP server the listing
cannot be answered at all.

Consolidation is Zarr's answer: one document at the store root holding a copy
of every metadata file in the tree. `zarr-tree` reads it when it is there, and
the whole walk — every node's metadata, and every group's children — comes out
of that one read. Two forms are read, the two that current zarr-python writes:
Zarr V2's `.zmetadata` at `zarr_consolidated_format` 1, and a Zarr V3 root
`zarr.json` carrying a `consolidated_metadata` block of `kind` `inline` with
`must_understand` false.

That is what makes a plain static HTTP server usable. Such a server answers
`GET` but not the WebDAV `PROPFIND` a listing needs, so without consolidation
the walk cannot get past the root; with it, no listing is wanted at all. See
[docs/remote-stores.md](docs/remote-stores.md#static-http-and-consolidated-metadata)
for a worked example against a real server.

Two properties matter more than the formats. **It is opportunistic**: a store
with no consolidated metadata, or with a form not read here, is walked exactly
as it was before — nothing that worked without consolidation comes to depend on
it. **It is all-or-nothing**: once the document has been read it is the only
thing read, because a consolidated document is a snapshot and a tree mixing it
with live reads would show two moments at once and mark neither. So a stale
snapshot is reported as it stands, unchecked.

The document formats, the filtering rules and the cost per store are in
[docs/zarr.md](docs/zarr.md#consolidated-metadata), and the overlay design in
[docs/architecture.md](docs/architecture.md#consolidated-metadata).

## Example output

Metadata that is missing, unreadable or malformed shows up in place rather than
stopping the walk. Below, `good` is a well-formed V3 array, `plain` is a V2
array whose `.zarray` omits `chunks` and `dtype`, and `truncated` has a
`zarr.json` that is not valid JSON:

```
$ zarr-tree broken.zarr
broken.zarr [group]
├─ zarr: V3
├── good [array]
│   ├─ zarr:   V3
│   ├─ shape:  [10]
│   ├─ chunks: [10]
│   └─ dtype:  float32
├── plain [array]
│   ├─ zarr:   V2
│   ├─ shape:  [10]
│   ├─ chunks: ?
│   └─ dtype:  ?
└── truncated [unknown]
```

A field that could not be read is printed as `?`. A directory whose metadata is
missing or not understood is labelled `[unknown]` and is still descended into.
The full degradation model — and how it differs from a store access error — is
in [docs/zarr.md](docs/zarr.md#unknown-and-malformed-nodes).

## Development

```sh
cargo check                  # type-check without linking; fastest feedback loop
cargo build                  # debug build -> target/debug/zarr-tree
cargo run -- <directory>     # build + run
cargo test                   # run all tests
cargo test -- --nocapture    # let tests print to stdout
cargo clippy --all-targets -- -D warnings  # lints, as CI runs them
cargo fmt --check            # formatting, as CI runs it
```

The suite is in two parts: 128 unit tests in `src/main.rs`, which cover metadata
parsing directly, and 26 integration tests in `tests/cli.rs`, which run the
compiled binary against throwaway fixture stores and assert on what it prints.
The Parquet fixtures are written by the same crate that reads them back, so
those tests run against real Parquet bytes with a real footer.

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test` on every push and pull request.

## Limitations

- Local directories, S3 and HTTP(S) only. There is no GCS or Azure backend, no
  ZIP store, and no writing of any kind.
- An HTTP(S) tree needs either consolidated metadata or a server that answers
  WebDAV `PROPFIND`. With neither, only `--depth 0` works: HTML directory-index
  pages are deliberately not scraped.
- Only the two consolidation forms current zarr-python writes are read: V2's
  `.zmetadata` at `zarr_consolidated_format` 1, and a V3 `consolidated_metadata`
  block of `kind` `inline` with `must_understand` false. Anything else is left
  alone and the store is read directly.
- Consolidated metadata is a snapshot and may be stale. `zarr-tree` does not
  check it against the store, and does not fall back to the store node by node
  when it disagrees — see [Consolidated metadata](#consolidated-metadata).
- HTTP(S) access is anonymous. Credentials, custom headers and client
  certificates are not supported; a query string on the URL is passed through,
  and that is all.
- A WebDAV listing costs one `PROPFIND` per group, but a server that redirects
  a collection URL to its trailing-slash form — Apache `mod_dav` does — turns
  that into two requests. Nothing here follows up on that.
- S3 credentials come from `AWS_*` environment variables, a web-identity token,
  a container credential endpoint or EC2 instance metadata. `~/.aws/credentials`
  is not read, and there is no `--profile`, `--region` or `--endpoint` flag:
  the environment is the whole interface.
- A bucket in a region other than `us-east-1` needs `AWS_REGION` set. Nothing
  discovers the region for you, and S3 answers a request to the wrong one with
  a redirect that is reported as a failed request.
- A remote store is classified with up to three `GetObject` calls per node —
  `.zgroup`, `.zarray`, `zarr.json`, in that order — so a Zarr V3 store pays two
  misses per node. Nothing is cached between runs and nothing is fetched
  concurrently: requests go out one at a time.
- A remote read that fails mid-walk is indistinguishable from a missing file, so
  a node whose metadata could not be fetched is reported `[unknown]` rather than
  as an error. Only the root is checked properly, before anything is printed.
- Pointing at a prefix that is not a Zarr node lists it to find out whether it
  exists at all. On a prefix holding a very large number of loose objects that
  listing is paginated to the end.
- Only `shape`, `chunks`/`chunk_shape`, `dtype`/`data_type`, the shard shape,
  a V3 array's `dimension_names`, `fill_value` and the codec chain are read. A
  fill value is displayed as stored and never interpreted or checked against
  the dtype, a codec chain is listed by name with no configuration shown and
  nothing ever run, user attributes only on request and only as stored (see
  `--attributes`), V2 dtypes are passed through as stored, and a V3 dtype in
  object form is reported by its name alone — see
  [docs/zarr.md](docs/zarr.md#deliberately-not-implemented).
- OME-Zarr support goes no further than spotting image, plate and well groups
  and showing their version, and for an image its axis names, declared pyramid
  level count and dataset paths. Coordinate transformations, `omero`, axis
  `type` and `unit`, acquisitions and field-of-view indices are not read, no
  scale factor or pixel size is calculated, and `image-label` is read only for
  its presence — the full list is in
  [docs/ome-zarr.md](docs/ome-zarr.md#current-limitations).
- SpatialData support goes no further than recognising a store root and its
  image, labels, points, shapes and table elements, summarising the Parquet
  payload of a points or shapes element from its footer, and summarising the
  AnnData metadata of a table. Element axes, feature keys, geometry types and
  coordinate transformations are not shown, and no element is joined to any
  other — a table names the regions it annotates, and outside `--validate`
  nothing checks that those elements exist or links them to the table.
- No Parquet record is ever read, and no expression value, annotation value,
  category or index label is. Row counts and schemas are what a footer
  *declares*; a sparse `X` reports no dtype and no non-zero count; GeoParquet
  semantics, `layers`, `obsm`, `uns` and the rest are not interpreted; and H5AD
  is not read. The full list is in
  [docs/spatialdata.md](docs/spatialdata.md#current-limitations).
- A segmentation that omits the optional `image-label` key is reported as an
  image. Nothing inside `image-label` — colours, properties, the source image —
  is read, and no label value is ever looked at.
- Symlinks are not followed, and a symlinked directory is not listed at all:
  the walk keeps only entries the filesystem reports as real directories.
  That is also what stops a link pointing back at an ancestor from looping.
- A directory that cannot be read ends the walk. The error names the reason
  but not the path, and there is no way to skip past it: the text output
  keeps whatever it had already printed, while `--json` prints nothing at
  all, since the whole document is built before any of it is written.
- `--validate` is a structural check over the metadata this tool already reads,
  and nothing more: it validates no document against a schema and knows no rule
  beyond the seven it ships with.
- Command-line limitations — no configuration file, no filtering or colour, no
  per-rule or per-severity validation filter, `--json` built whole in memory,
  and the `-`-prefixed and non-UTF-8 argument cases — are listed in
  [docs/cli.md](docs/cli.md#current-limitations).

## Roadmap

Direction, with nothing promised, lives in [docs/roadmap.md](docs/roadmap.md).
Near term it is documentation work, more structural validation within the
existing model, and better OME-Zarr presentation. GCS and Azure backends, and
anything beyond lightweight OME-Zarr and SpatialData recognition, remain out of
scope.

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) covers
the development setup, the quality gate CI runs, and the design constraints —
read-only, metadata-only, arrays are leaves — that a change is expected to
preserve. [CHANGELOG.md](CHANGELOG.md) records what has changed between
releases, and [SECURITY.md](SECURITY.md) covers vulnerability reporting, which
does not go through public issues.

## Why this project exists

It started as a Rust learning project. The goal was something small and real:
walk a directory, read a little JSON, and print it clearly — with each step
adding one idea (`Option` and `?`, borrowing, enums with data, unit tests)
rather than reaching for a framework.

It is also genuinely useful. Zarr stores are directory trees full of chunk
files, and `ls` or `tree` buries the structure in thousands of chunk keys.
`zarr-tree` shows the part that matters. The scope is deliberately small, and
the code is meant to stay readable.
