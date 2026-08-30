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
├── images [group]
│   └── morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]
│       ├─ axes: c, y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── labels [group]
│   └── nuclei [group, OME-Zarr 0.5-dev-spatialdata, SpatialData labels]
│       ├─ axes: y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── points [group]
│   └── transcripts [group, SpatialData points]
├── shapes [group]
│   └── cell_boundaries [group, SpatialData shapes]
└── tables [group]
    └── table [group, SpatialData table]
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

`--validate` was added after v0.3.0 and is currently on `master` only — build
from source to use it. Everything else above is in v0.3.0.

[docs/getting-started.md](docs/getting-started.md) covers this at more length,
and [docs/remote-stores.md](docs/remote-stores.md) covers the S3 and HTTP cases.

## What it understands

| Area | Support |
| --- | --- |
| Zarr | V2 and V3, sharding, consolidated metadata |
| OME-Zarr | 0.3–0.5, multiscales, axes, dataset paths, HCS plates and wells |
| SpatialData | store root, images, labels, points, shapes, tables |
| Parquet | footer summaries only — rows, columns, file count, schema |
| AnnData | metadata summaries only — no value is read, nothing is counted |
| Storage | local filesystem, S3, HTTP/WebDAV, static HTTP via consolidated metadata |
| Validation | metadata-only structural checks, on `master` after v0.3.0 |

[docs/status.md](docs/status.md) carries the exact matrix, including what is
deliberately not implemented.

## Project status

The latest release is [v0.3.0](https://github.com/ronfinn/zarr-tree/releases).
Development continues on `master`, which currently carries metadata structural
validation (`--validate`) added after that release.

| | |
| --- | --- |
| Latest release | v0.3.0 |
| Development HEAD | post-v0.3.0, `--validate` present |
| Tests | 112 passing — 94 unit, 18 integration |
| Minimum supported Rust version | 1.88 |

This is a small utility maintained by one person, not a certified product. The
scope is deliberately narrow and the [non-goals](docs/status.md#explicit-non-goals)
are as much a part of the design as the features.

## Documentation

- [Getting started](docs/getting-started.md) — build it, inspect a first store,
  and the whole option set in one page.
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

The general CLI reference — usage, `--depth`, `--json` and `--validate` — is
still below, and is being split out of this file one subject at a time.

## Features

- Prints a tree of a Zarr store from a directory, an `s3://` URI or an
  `http(s)://` URL — the same walk, the same output and the same options
  wherever the bytes are.
- Reads Zarr V2 and V3 metadata layouts, labelling each node `[group]`,
  `[array]` or `[unknown]`, and showing `shape`, `chunks` and `dtype` under
  every array.
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
| Consolidation | `.zmetadata` | inline `consolidated_metadata` |

V2 is checked first, so a directory carrying both is reported as V2. V2 dtypes
are printed exactly as stored — `<u2`, `|u1` — and are never translated. A
sharded V3 array reports `chunks` (the inner chunk shape) and `shards` (the
chunk grid) under separate names:

```
$ zarr-tree sharded.zarr
sharded.zarr [group]
└── img [array]
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
├─ axes: c, y, x
├─ pyramid levels: 3
├─ datasets: 0, 1, 2
├── 0 [array]
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
listed under [Validation](#validation); for conformance checking use the
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
├── images [group]
│   └── morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]
│       ├─ axes: c, y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── labels [group]
│   └── nuclei [group, OME-Zarr 0.5-dev-spatialdata, SpatialData labels]
│       ├─ axes: y, x
│       ├─ pyramid levels: 1
│       └─ datasets: s0
├── points [group]
│   └── transcripts [group, SpatialData points]
│       ├─ rows: 3,714
│       ├─ columns: 4
│       ├─ parquet files: 2
│       └─ schema: x:double, y:double, feature_name:string, cell_id:int32
├── shapes [group]
│   └── cell_boundaries [group, SpatialData shapes]
│       ├─ rows: 1,200
│       ├─ columns: 2
│       ├─ parquet files: 1
│       └─ schema: geometry:byte_array, cell_id:int32
└── tables [group]
    └── table [group, SpatialData table]
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
zarr-tree [OPTIONS] <store>
```

The store is a directory, an `s3://` URI or an `http(s)://` URL, and the walk,
the tree and the options are the same for all three:

```sh
zarr-tree /data/example.zarr
zarr-tree s3://bucket/example.zarr
zarr-tree https://example.org/example.zarr
```

Exactly one store, plus any of the options below in any order. Anything else
names what was wrong and exits with status 1:

```
$ zarr-tree
error: expected a store
usage: zarr-tree [OPTIONS] <STORE>

$ zarr-tree --depth two store.zarr
error: --depth needs a whole number, not "two"
usage: zarr-tree [OPTIONS] <STORE>
```

```
        --depth <N>  Descend at most N levels below the root
        --json       Print the same tree as JSON
        --validate   Check the structure the metadata declares
    -h, --help       Print help
    -V, --version    Print version
```

Exit status:

| Status | Meaning |
| --- | --- |
| 0 | The store was walked. With `--validate`, nothing worse than a `WARN`. |
| 1 | The store could not be read, or the command line made no sense. |
| 2 | `--validate` ran and reported at least one `ERROR`. |

An argument beginning with `-` that is not one of these is read as a mistyped
option rather than as a path, so a directory whose name starts with `-` cannot
be inspected.

### S3

A store argument beginning with `s3://` is read from S3 instead of from disk.
Nothing else changes: the same walk, the same tree, the same `--depth` and
`--json`.

```
$ zarr-tree --depth 1 s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr
s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr [group]
├── mapping [group]
├── recon-1 [group]
└── recon-2 [group]
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
└── fibsem-uint16 [group, OME-Zarr 0.4]
    ├─ axes: z, y, x
    ├─ pyramid levels: 6
    ├─ datasets: s0, s1, s2, s3, s4, s5
    ├── s0 [array]
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
├─ axes: y, x
├─ pyramid levels: 1
├─ datasets: 0
├── 0 [array]
│   ├─ shape:  [1024, 1024]
│   ├─ chunks: [128, 128]
│   ├─ shards: [512, 512]
│   └─ dtype:  uint16
└── labels [group]
    └── cells [array]
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

### Depth

`--depth N` limits how far below the root the walk goes. `0` shows the root on
its own, `1` adds its direct children, and so on. Left out, the whole store is
walked, exactly as before the option existed.

```
$ zarr-tree --depth 1 experiment.zarr
experiment.zarr [group, SpatialData 0.2]
├── images [group]
├── labels [group]
├── points [group]
├── shapes [group]
└── tables [group]
```

A node that is shown keeps its own metadata rows: those describe the node
itself, not anything below it. So an image group at the limit still shows its
axes, pyramid levels and dataset paths, even though its resolution arrays are
one level too far to appear.

Arrays are leaves at any depth. The limit never has anything to say about them,
because the walk already stops there.

At the limit the directory is not read at all, which is what makes `--depth 0`
cheap on a store with a million chunk files.

### JSON

`--json` prints the same walk as one JSON document, for piping into `jq` or
reading from a script. It combines with `--depth`.

```
$ zarr-tree --json --depth 1 experiment.zarr
{
  "children": [
    {
      "children": [],
      "kind": "group",
      "name": "images"
    }
  ],
  "kind": "group",
  "name": "experiment.zarr",
  "spatialdata": {
    "kind": "root",
    "version": "0.2"
  }
}
```

Every node has three fields, and then a section for each kind of metadata that
applies to it:

| Field | Always | Meaning |
| --- | --- | --- |
| `name` | yes | The directory name. On the root, the path as it was typed — the same thing the tree's first line shows. |
| `kind` | yes | `group`, `array` or `unknown` |
| `children` | yes | The child nodes, in the same order the tree lists them. Empty for an array, and empty at the depth limit. |
| `array` | arrays only | `shape`, `chunks`, `dtype`, and `shards` on a sharded V3 array |
| `ome` | OME-Zarr groups | `tag`, `kind` (`image`, `plate`, `well`), `version`, `axes`, `pyramid_levels`, `datasets`, and `rows`, `columns`, `wells` on a plate |
| `spatialdata` | SpatialData nodes | `kind` (`root`, `image`, `labels`, `points`, `shapes`, `table`) and `version`, which only a store root records; on a table also `regions`, `region_key` and `instance_key` |
| `parquet` | points and shapes elements with a readable payload | `rows`, `columns`, `files` and the whole `schema` |
| `anndata` | SpatialData tables | `encoding_version`, `observations`, `variables`, `x`, and the declared `obs_columns` and `var_columns` in full |

A section is absent when that kind of metadata does not apply; a field inside a
section is `null` when the file gave no readable value. That is the same rule
the tree follows when it prints `?`:

```
$ zarr-tree --json partial.zarr | jq '.children[0].array'
{
  "chunks": null,
  "dtype": null,
  "shape": [1024, 1024]
}
```

`shape` and `chunks` are real JSON arrays rather than the `[1024, 1024]` text
the tree draws, and their entries are copied across exactly as stored — a
malformed `"shape": [1, "x"]` comes out as `[1, "x"]` rather than being
dropped.

`spatialdata` and `anndata` stay separate objects on a table, because they are
two vocabularies read from two sets of keys: what SpatialData said about the
elements the table annotates, and what AnnData said about the table itself.

```
$ zarr-tree --json xenium.zarr/tables/table --depth 0 | jq '.anndata'
{
  "encoding_version": "0.1.0",
  "obs_columns": ["cell_id", "transcript_counts", "…"],
  "observations": 167780,
  "var_columns": ["gene_ids", "feature_types", "genome"],
  "variables": 313,
  "x": {
    "kind": "csr",
    "shape": [167780, 313]
  }
}
```

The two outputs come from one reading of the metadata: `--json` is a second
renderer, not a second interpretation, so the tree and the document cannot
disagree about what a store contains. Object keys come out in alphabetical
order, which is why `children` leads; that is `serde_json`'s default and
keeping it avoids a dependency for nothing.

### Validation

`--validate` checks what a store's metadata *declares* against what the store
*has*, and prints findings instead of the tree. It reads metadata only — the
same files the tree reads, plus the four an AnnData table names — and opens no
chunk, no Parquet record and no expression value.

```
$ zarr-tree --validate experiment.zarr
PASS  /  Zarr root metadata is readable
PASS  /images/morphology  OME dataset path "0" exists
PASS  /images/morphology  OME dataset path "1" exists
PASS  /images/morphology  pyramid levels agree with the multiscale's axes on 3 dimensions
WARN  /points/transcripts  SpatialData points payload metadata unavailable
ERROR /tables/table  table region "cells" does not name an existing SpatialData element

Validation: 4 passed, 1 warning, 1 error
```

Each line is a severity, the node the finding is about, and what was found.
The three severities mean three different things, and the middle one carries
the weight:

| Severity | Meaning |
| --- | --- |
| `PASS` | The structure the metadata declares is there. |
| `WARN` | The check could not be made. Nothing is claimed either way. |
| `ERROR` | The metadata declares something the store does not have. |

A points payload on a static HTTP server cannot be listed, so it cannot be
inspected — that is a `WARN`, not a broken store. The same goes for a shape a
file did not record, or an index length that could not be read.

The rules, all of them over metadata this tool already reads:

1. **Zarr metadata.** Every node this program walked into could be identified,
   and each array's `shape`, `chunks` and — on a sharded V3 array — `shards`
   agree on how many dimensions there are. Codecs are not checked.
2. **OME-Zarr dataset paths.** Every `multiscales[0].datasets[].path` names a
   node that exists and is an array.
3. **Pyramid dimensions.** Every resolution level has the same number of
   dimensions, and the same number as the multiscale declares axes. No scale,
   resolution or downsampling factor is looked at.
4. **HCS wells.** Every path in a plate's `wells` list names a group that
   exists. Acquisitions and fields of view are not checked.
5. **SpatialData table regions.** Every element named in a table's `region`
   exists as a recognised image, labels, points or shapes element. No name is
   inferred from a payload.
6. **AnnData dimensions.** `X.shape[0]` matches the length the `obs` index
   declares, and `X.shape[1]` the `var` index. The index lengths come from the
   index arrays' own metadata; no value is read and nothing is counted.
7. **Parquet availability.** A points or shapes payload that is there and
   readable passes; one that is there and could not be inspected warns. A
   payload that is genuinely absent is not a finding.

`--validate` combines with `--json` and prints one document — the same findings
in the same order, with the counts the summary line carries:

```
$ zarr-tree --validate --json experiment.zarr | jq '.summary'
{
  "errors": 1,
  "passed": 4,
  "warnings": 1
}
```

Every finding has three fields: `severity` (`pass`, `warn` or `error`), `path`
and `message`.

It does not combine with `--depth`, and says so rather than picking one:

```
$ zarr-tree --validate --depth 1 store.zarr
error: --depth cannot be combined with --validate
usage: zarr-tree [OPTIONS] <STORE>
```

A walk that stopped early would report every node below the limit as missing —
a dataset path, a well, a region — so the two options cannot both mean what
they say at once.

This is a structural check and not a specification conformance pass. Nothing
here validates a Zarr, OME-NGFF or SpatialData document against its schema, and
the ordinary `zarr-tree` output is unchanged: it still prints what the metadata
says, unchecked.

### Pipelines

A large store prints thousands of lines, so `zarr-tree` is built to sit at the
producing end of a pipe:

```sh
zarr-tree big.zarr | head
zarr-tree big.zarr | less
zarr-tree big.zarr | grep SpatialData
```

When the reader stops reading, the write fails with `BrokenPipe` and the run
ends there — quietly, with nothing on stderr and exit status 0. The reader
said it had seen enough, and that is not an error to report.

Every other failure keeps its own behaviour: a line on stderr and exit status
1. A `--validate` run that found an `ERROR` exits 2 — see
[Validation](#validation).

## Example output

Metadata that is missing, unreadable or malformed shows up in place rather than
stopping the walk. Below, `good` is a well-formed V3 array, `plain` is a V2
array whose `.zarray` omits `chunks` and `dtype`, and `truncated` has a
`zarr.json` that is not valid JSON:

```
$ zarr-tree broken.zarr
broken.zarr [group]
├── good [array]
│   ├─ shape:  [10]
│   ├─ chunks: [10]
│   └─ dtype:  float32
├── plain [array]
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

The suite is in two parts: 94 unit tests in `src/main.rs`, which cover metadata
parsing directly, and 18 integration tests in `tests/cli.rs`, which run the
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
- Only `shape`, `chunks`/`chunk_shape`, `dtype`/`data_type` and the shard
  shape are read. Compressors, fill values, dimension names and user
  attributes are not shown, `codecs` is read for the sharding codec alone, V2
  dtypes are passed through as stored, and V3 dtypes given in object form are
  not interpreted — see
  [docs/zarr.md](docs/zarr.md#deliberately-not-implemented).
- No output options beyond `--depth`, `--json` and `--validate`: no filtering,
  no colour.
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
- Arguments are read with `std::env::args`, which panics on an argument that
  is not valid UTF-8. On a system where such a path can exist, that happens
  before any argument validation runs, so the failure is a panic rather than
  the usual message and exit status 1.
- `--json` builds the whole document in memory before writing any of it, while
  the text output is streamed as the walk proceeds. So peak memory grows with
  the number of nodes in the tree, and an error part-way through a walk
  produces no JSON at all rather than a partial document.
- `--validate` is a structural check over the metadata this tool already reads,
  and nothing more. It validates no document against a schema, knows no rule
  the seven listed under [Validation](#validation) do not cover, cannot be
  limited to one rule or one severity, and holds the whole node map in memory
  because a table's region may name an element anywhere in the store. It walks
  the store whole and so cannot be combined with `--depth`.

## Roadmap

Direction, with nothing promised, lives in [docs/roadmap.md](docs/roadmap.md).
Near term it is documentation work, more structural validation within the
existing model, and better OME-Zarr presentation. GCS and Azure backends, and
anything beyond lightweight OME-Zarr and SpatialData recognition, remain out of
scope.

## Why this project exists

It started as a Rust learning project. The goal was something small and real:
walk a directory, read a little JSON, and print it clearly — with each step
adding one idea (`Option` and `?`, borrowing, enums with data, unit tests)
rather than reaching for a framework.

It is also genuinely useful. Zarr stores are directory trees full of chunk
files, and `ls` or `tree` buries the structure in thousands of chunk keys.
`zarr-tree` shows the part that matters. The scope is deliberately small, and
the code is meant to stay readable.
