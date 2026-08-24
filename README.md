# zarr-tree

[![CI](https://github.com/ronfinn/zarr-tree/actions/workflows/ci.yml/badge.svg)](https://github.com/ronfinn/zarr-tree/actions/workflows/ci.yml)

A small Rust CLI for exploring the structure and metadata of local Zarr stores.

```
$ zarr-tree example.zarr
example.zarr [group]
├── labels [group]
│   └── cells [array]
│       ├─ shape:  [1, 2048, 2048]
│       ├─ chunks: [1, 512, 512]
│       └─ dtype:  <u4
└── raw [group]
    ├── 0 [array]
    │   ├─ shape:  [1, 2048, 2048]
    │   ├─ chunks: [1, 512, 512]
    │   └─ dtype:  uint16
    └── 1 [array]
        ├─ shape:  [1, 1024, 1024]
        ├─ chunks: [1, 512, 512]
        └─ dtype:  uint16
```

## Features

- Prints a directory tree of a local Zarr store.
- Labels each directory as `[group]`, `[array]` or `[unknown]`.
- Shows `shape`, `chunks` and `dtype` underneath every array.
- Stops descending at arrays, so chunk storage (a V3 `c/` directory, V2 chunk
  keys) does not clutter the output.
- Recognises OME-Zarr image groups and tags them, e.g. `[group, OME-Zarr 0.4]`.
- Shows an OME-Zarr image's axis names on one row, e.g. `axes: c, y, x`.
- Summarises an OME-Zarr multiscale pyramid: how many resolution levels the
  metadata declares, and the paths it declares them at.
- Recognises the root of a SpatialData store and tags it, e.g.
  `[group, SpatialData 0.2]`.
- Recognises SpatialData elements — images, labels, points, shapes and tables —
  e.g. `[group, SpatialData points]`, and tells a segmentation from an image by
  its OME-Zarr metadata rather than by its directory name.
- Limits how far it descends with `--depth N`.
- Prints the same walk as JSON with `--json`, for `jq` and scripts.
- Sits quietly at the producing end of a pipe: `| head` ends the run with no
  panic and no error.
- Degrades gracefully: metadata that cannot be read or parsed costs only that
  node's label or a single field, never the rest of the walk.

## Supported Zarr versions

Both Zarr V2 and V3 metadata layouts are recognised:

| Version | Group marker | Array marker | Fields read |
| --- | --- | --- | --- |
| V2 | `.zgroup` | `.zarray` | `shape`, `chunks`, `dtype` |
| V3 | `zarr.json` with `"node_type": "group"` | `zarr.json` with `"node_type": "array"` | `shape`, `chunk_grid.configuration.chunk_shape`, `data_type` |

V2 is checked first, so a directory carrying both V2 and V3 metadata is reported
as V2.

V2 `dtype` values are displayed exactly as stored, in NumPy notation — `<u2`,
`|u1`, `<M8[ns]`. They are not translated into V3 names such as `uint16`, and no
attempt is made to validate them.

## OME-Zarr

An [OME-Zarr](https://ngff.openmicroscopy.org/) image is an ordinary Zarr group
whose attributes carry a `multiscales` key, describing the same image stored at
several resolutions. Groups like that are tagged, with the metadata version
appended when one is recorded:

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
├── 1 [array]
│   ├─ shape:  [2, 1024, 1024]
│   ├─ chunks: [1, 512, 512]
│   └─ dtype:  <u2
└── 2 [array]
    ├─ shape:  [2, 512, 512]
    ├─ chunks: [1, 512, 512]
    └─ dtype:  <u2
```

Where the metadata lives follows the Zarr version:

| OME-Zarr | Zarr | Attributes | Version field |
| --- | --- | --- | --- |
| 0.1 - 0.4 | V2 | `.zattrs`, keys at the top level | first `multiscales` entry's `version`, often absent |
| 0.5 | V3 | `attributes.ome` inside `zarr.json` | `attributes.ome.version` |

The version is printed exactly as stored and is never checked against the
versions that exist, so an unfamiliar one still shows. A group whose
`multiscales` is present but carries no readable version is tagged
`[group, OME-Zarr]`.

Axis names come from the first `multiscales` entry's `axes`, whose form changed
over the course of the specification:

| OME-Zarr | `axes` |
| --- | --- |
| 0.1, 0.2 | no `axes` field |
| 0.3 | a list of names, `["c", "y", "x"]` |
| 0.4, 0.5 | a list of objects, `{"name": "y", "type": "space", "unit": "micrometer"}` |

Both forms are read, and only `name` is displayed. An entry whose name cannot be
read shows as `?`, so the number of axes always matches the number the file
declares. Axes that are absent, empty or not a list print no row at all —
nothing is inferred from an array's dimensionality.

### Pyramid levels

The same `multiscales` entry's `datasets` lists the resolution levels the image
is stored at. Unlike `axes`, this has had the same shape since OME-Zarr 0.1 — a
list of objects each carrying a `path` — so one reading serves every version.
What 0.4 added to each entry, `coordinateTransformations`, is not read.

Two rows come from it: how many levels are declared, and the paths they are
declared at.

```
├─ pyramid levels: 3
├─ datasets: 0, 1, 2
```

The count comes from the metadata, **never from counting child directories**.
The two often disagree: an image group commonly holds a `labels` group beside
its levels, a 0.5 store adds an `OME` directory, a path may be nested such as
`a/b`, and a truncated copy may declare more levels than it actually contains.
The directories are already listed below these rows; what the metadata claims is
the part you cannot otherwise see.

Paths are printed exactly as stored. `0`, `1`, `2` is only a convention —
`s0`, `full`, `half` and nested paths are all legal — so nothing is sorted,
renumbered or interpreted:

```
$ zarr-tree named.zarr
named.zarr [group, OME-Zarr 0.3]
├─ axes: y, x
├─ pyramid levels: 2
├─ datasets: full, half
├── full [array]
...
```

An entry whose path cannot be read shows as `?`, so the count still matches what
the file declares. `datasets` that is absent, empty or not a list prints neither
row:

```
$ zarr-tree partial.zarr
partial.zarr [group, OME-Zarr 0.4]
├─ axes: y, x
├─ pyramid levels: 3
└─ datasets: 0, ?, 2
```

That store declares three levels but has none of them on disk, so nothing
follows the rows and the last one closes the branch with `└─`.

Nothing checks that a declared path exists on disk, and no scale factors, pixel
sizes or physical extents are calculated — the `coordinateTransformations` those
would come from are not read at all.

This is **recognition, not validation**. Nothing here checks a store against the
OME-NGFF specification: axes, dataset paths, coordinate transformations, `omero`
channels, labels and plate/well layouts are never validated, and most of them
are not read at all. For real validation use the
[OME-NGFF validator](https://ome.github.io/ome-ngff-validator/).

## SpatialData

[SpatialData](https://spatialdata.scverse.org/) keeps a spatial omics
experiment in a Zarr container: microscopy images, segmentation masks,
transcript locations, geometries and annotation tables, all in one store. The
root of such a store is tagged, with the container format version appended when
one is recorded:

```
$ zarr-tree experiment.zarr
experiment.zarr [group, SpatialData 0.2]
├── images [group]
│   └── morphology [group, OME-Zarr 0.5-dev-spatialdata]
│       ├─ axes: c, y, x
│       ├─ pyramid levels: 1
│       ├─ datasets: s0
│       └── s0 [array]
│           ├─ shape:  [4, 2048, 2048]
│           ├─ chunks: [1, 512, 512]
│           └─ dtype:  uint16
├── points [group]
├── shapes [group]
└── tables [group]
```

The container format version also decides which Zarr layout the store uses, and
so where the marker lives:

| Container | Zarr | Attributes |
| --- | --- | --- |
| 0.1 | V2 | `.zattrs`, keys at the top level |
| 0.2 | V3 | `attributes` inside `zarr.json` |

Detection requires `spatialdata_attrs.spatialdata_software_version`, not merely
the presence of `spatialdata_attrs`. The elements inside a store — images,
labels, points, shapes — each carry a `spatialdata_attrs` of their own, holding
just the version of their own encoding; only the root records the software that
wrote it. Without that distinction every element would be reported as a store
of its own, which is why `morphology` above is tagged as the OME-Zarr image it
is and not as a second SpatialData root.

The directory names `images`, `labels`, `points`, `shapes` and `tables` are
never used to detect anything. In a real store those groups carry no attributes
at all, so the name would be the only thing left to go on — and an ordinary
Zarr store whose children happen to be called `images` and `points` is not a
SpatialData store:

```
$ zarr-tree plain.zarr
plain.zarr [group]
├── images [group]
├── points [group]
├── shapes [group]
└── tables [group]
```

The version is printed exactly as stored and is never checked against the
versions that exist. A root whose marker is present but carries no readable
version is tagged `[group, SpatialData]`.

## Installation

From source:

```sh
git clone https://github.com/ronfinn/zarr-tree.git
cd zarr-tree
cargo build --release
```

The binary lands at `target/release/zarr-tree`. Copy it somewhere on your
`PATH`, or install it into `~/.cargo/bin`:

```sh
cargo install --path .
```

Rust 1.85 or newer is required, as the crate uses edition 2024. It was developed
with rustc 1.98.0.

## Usage

```sh
zarr-tree [OPTIONS] <directory>
```

Exactly one directory, plus any of the options below in any order. Anything
else names what was wrong and exits with status 1:

```
$ zarr-tree
error: expected a directory
usage: zarr-tree [OPTIONS] <DIRECTORY>

$ zarr-tree --depth two store.zarr
error: --depth needs a whole number, not "two"
usage: zarr-tree [OPTIONS] <DIRECTORY>
```

```
        --depth <N>  Descend at most N levels below the root
        --json       Print the same tree as JSON
    -h, --help       Print help
    -V, --version    Print version
```

An argument beginning with `-` that is not one of these is read as a mistyped
option rather than as a directory, so a directory whose name starts with `-`
cannot be inspected.

`--depth N` limits how far below the root the walk goes. `0` shows the root on
its own, `1` adds its direct children, and so on. Left out, the whole store is
walked. A node that is shown keeps its own metadata rows, and arrays are leaves
at any depth.

`--json` prints the same walk as one JSON document, for piping into `jq` or
reading from a script. It combines with `--depth`. Every node carries its
`name`, its `kind` and its `children`; the `array`, `ome` and `spatialdata`
sections appear only on the nodes they apply to, and a field inside one is
`null` where the tree would print `?`.

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

## Development

```sh
cargo check                  # type-check without linking; fastest feedback loop
cargo build                  # debug build -> target/debug/zarr-tree
cargo run -- <directory>     # build + run
cargo test                   # run all tests
cargo test -- --nocapture    # let tests print to stdout
cargo clippy -- -D warnings  # lints, as CI runs them
cargo fmt --check            # formatting, as CI runs it
```

The suite is in two parts: 37 unit tests in `src/main.rs`, which cover metadata
parsing directly, and 11 integration tests in `tests/cli.rs`, which run the
compiled binary against throwaway fixture stores and assert on what it prints.

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` on
every push and pull request.

## Limitations

- Local filesystem paths only. No remote or object-store access.
- Only `shape`, `chunks`/`chunk_shape` and `dtype`/`data_type` are read.
  Codecs, compressors, fill values, dimension names and user attributes are
  not shown.
- No output options beyond `--depth` and `--json`: no filtering, no colour.
- V2 dtypes are passed through as stored and V3 dtypes given in object form
  (the extension syntax) are not interpreted.
- Sharding is not understood; a sharded array shows its declared chunk shape
  only.
- OME-Zarr support goes no further than spotting image groups and showing their
  version, axis names, declared pyramid level count and dataset paths. Axis
  `type` and `unit` are not shown, nothing is validated (axis names, ordering or
  count; whether a declared dataset path exists), and coordinate
  transformations, `omero` and plate/well metadata are not read. `image-label`
  is read only for its presence, to tell a segmentation from an image. No
  scale factors, pixel sizes or physical extents are calculated.
- SpatialData support goes no further than recognising a store root and its
  image, labels, points, shapes and table elements. Nothing inside an element
  is read: `points.parquet`, `shapes.parquet` and a table's `X`, `obs`, `var`
  and `layers` are left alone.
- A segmentation that omits the optional `image-label` key is reported as an
  image. Nothing inside `image-label` — colours, properties, the source image —
  is read, and no label value is ever looked at.
- A store root written before SpatialData recorded a software version carries
  no root marker and is not recognised as one; its points, shapes and table
  elements still are, because those name themselves in a key such a store does
  carry. Its images and labels do not, since they are recognised in part by a
  `spatialdata_attrs` those older stores do not write. Nothing is inferred from
  directory names in any case.
- Symlinked directories are listed but not followed.

## Roadmap

Small, in roughly this order:

1. Show a node's user attributes when asked.
2. Report V3 dtypes given in object form, instead of showing them as missing.

Remote stores, and anything beyond lightweight OME-Zarr and SpatialData
recognition, are out of scope for now.

## Why this project exists

It started as a Rust learning project. The goal was something small and real:
walk a directory, read a little JSON, and print it clearly — with each step
adding one idea (`Option` and `?`, borrowing, enums with data, unit tests)
rather than reaching for a framework.

It is also genuinely useful. Zarr stores are directory trees full of chunk
files, and `ls` or `tree` buries the structure in thousands of chunk keys.
`zarr-tree` shows the part that matters. The scope is deliberately small, and
the code is meant to stay readable.
