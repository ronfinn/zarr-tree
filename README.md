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
one is recorded, and so are the elements inside it:

```
$ zarr-tree experiment.zarr
experiment.zarr [group, SpatialData 0.2]
├── images [group]
│   └── morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]
│       ├─ axes: c, y, x
│       ├─ pyramid levels: 1
│       ├─ datasets: s0
│       └── s0 [array]
│           ├─ shape:  [4, 2048, 2048]
│           ├─ chunks: [1, 512, 512]
│           └─ dtype:  uint16
├── labels [group]
│   └── nuclei [group, OME-Zarr 0.5-dev-spatialdata, SpatialData labels]
│       ├─ axes: y, x
│       ├─ pyramid levels: 1
│       ├─ datasets: s0
│       └── s0 [array]
│           ├─ shape:  [2048, 2048]
│           ├─ chunks: [512, 512]
│           └─ dtype:  uint32
├── points [group]
│   └── transcripts [group, SpatialData points]
│       └── points.parquet [unknown]
├── shapes [group]
│   └── cell_boundaries [group, SpatialData shapes]
└── tables [group]
    └── table [group, SpatialData table]
        ├── X [array]
        │   ├─ shape:  [1200, 313]
        │   ├─ chunks: [1200, 313]
        │   └─ dtype:  float32
        ├── obs [group]
        └── var [group]
```

### The store root

The container format version also decides which Zarr layout the store uses, and
so where every marker below lives:

| Container | Zarr | Attributes |
| --- | --- | --- |
| 0.1 | V2 | `.zattrs`, keys at the top level |
| 0.2 | V3 | `attributes` inside `zarr.json` |

Root detection requires `spatialdata_attrs.spatialdata_software_version`, not
merely the presence of `spatialdata_attrs`. The elements inside a store each
carry a `spatialdata_attrs` of their own, holding just the version of their own
encoding; only the root records the software that wrote it. Without that
distinction every element would be reported as a store of its own.

### Elements

Points, shapes and tables name their own kind as a plain string in their
attributes:

| Element | Key | Value |
| --- | --- | --- |
| points | `encoding-type` | `ngff:points` |
| shapes | `encoding-type` | `ngff:shapes` |
| table | `spatialdata-encoding-type` | `ngff:regions_table` |

Two key names, for a historical reason rather than a semantic one: a table's
group is written by AnnData, which claims `encoding-type` for its own
`"anndata"`, so SpatialData records the kind one key over.

Each value is matched exactly, never by prefix or by the presence of a key
alone. AnnData writes `encoding-type` throughout the subtree beneath a table —
`"dataframe"`, `"csr_matrix"`, `"array"` — and none of those is an element.

Naming a table does not collapse it. The AnnData subtree below it is an
ordinary Zarr hierarchy of groups and arrays, and it is walked and printed like
any other. Recognising a node and deciding what to show underneath it are
separate questions; nothing inside a table — `X`, `obs`, `var`, `layers`, the
sparse matrix components, the region it annotates — is read or interpreted.

Rasters name themselves nowhere: SpatialData writes them through the OME-Zarr
writers, which have no `encoding-type` of their own. They are recognised from
two facts together:

| Element | SpatialData's mark | OME-Zarr metadata |
| --- | --- | --- |
| image | `spatialdata_attrs` (an object) | `multiscales` |
| labels | `spatialdata_attrs` (an object) | `multiscales` **and** `image-label` |

Both halves are needed. `spatialdata_attrs` alone is weak evidence — it is the
same object a store root carries, minus the software version — but paired with
OME-Zarr image metadata it separates an element of a store from an ordinary
microscopy image that has nothing to do with SpatialData. Without it, every
OME-Zarr image ever written would be tagged as a SpatialData element:

```
$ zarr-tree plain-image.zarr
plain-image.zarr [group, OME-Zarr 0.4]
├─ axes: c, y, x
├─ pyramid levels: 1
└─ datasets: 0
```

`image-label` is an OME-NGFF construct, not a SpatialData one: it is an object
describing the colours and properties of the label values, and OME-NGFF places
it beside `multiscales` in the same metadata object. It is read for its
presence alone — no label value, colour or property is looked at. The
specification says a label image *should* carry it rather than *must*, so a
segmentation that omits it is reported as an image; the alternative would be to
guess from the `labels/` directory name.

Element tags carry no version. The number an element records is the version of
its own encoding, which is a different quantity from the container version on
the root line: in a container 0.2 store the points element is 0.2 and the
shapes element is 0.3, and printing those next to `SpatialData 0.2` would
suggest a disagreement that is not there.

Recognition does not depend on the format version. Both markers have been
written unchanged since the earliest releases and are the same in Zarr V2 and
V3. What changed between element versions is where the *payload* lives — older
shapes kept their geometry in sibling Zarr arrays, newer ones in a GeoParquet
file — and no payload is read here.

Elements are also recognised independently of the root, so a store written
before SpatialData recorded a software version still has its elements tagged
even though its root cannot be.

Nothing beyond the kind is read. An element's axis names (which the writer
sorts, so they do not record dimension order), its feature and instance keys,
and its coordinate transformations are all left alone.

### Payload files

`points.parquet` shows up as `[unknown]`, and `shapes.parquet` does not show up
at all. Both are correct. Points are written as a *partitioned* Parquet
dataset, which on disk is a directory of `part.N.parquet` files, so the walk
finds it and reports honestly that it holds no Zarr metadata. Shapes are
written as a single Parquet *file*, and only directories are listed.

Neither is suppressed. That would mean matching on the name `points.parquet`,
and names decide nothing here — and in an older store the children of a shapes
element are genuine Zarr arrays worth showing.

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
| `array` | arrays only | `shape`, `chunks`, `dtype` |
| `ome` | OME-Zarr images | `tag`, `version`, `axes`, `pyramid_levels`, `datasets` |
| `spatialdata` | SpatialData nodes | `kind` (`root`, `image`, `labels`, `points`, `shapes`, `table`) and `version`, which only a store root records |

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

The two outputs come from one reading of the metadata: `--json` is a second
renderer, not a second interpretation, so the tree and the document cannot
disagree about what a store contains. Object keys come out in alphabetical
order, which is why `children` leads; that is `serde_json`'s default and
keeping it avoids a dependency for nothing.

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
1.

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
  and `layers` are left alone. Element axes, feature keys, instance keys,
  geometry types, the region a table annotates and coordinate transformations
  are not shown, and no element is joined to any other.
- A segmentation that omits the optional `image-label` key is reported as an
  image. Nothing inside `image-label` — colours, properties, the source image —
  is read, and no label value is ever looked at.
- A store root written before SpatialData recorded a software version carries
  no root marker and is not recognised as one; its points, shapes and table
  elements still are, because those name themselves in a key such a store does
  carry. Its images and labels do not, since they are recognised in part by a
  `spatialdata_attrs` those older stores do not write. Nothing is inferred from
  directory names in any case.
- Symlinks are not followed, and a symlinked directory is not listed at all:
  the walk keeps only entries the filesystem reports as real directories.
  That is also what stops a link pointing back at an ancestor from looping.
- A directory that cannot be read ends the walk. The error names the reason
  but not the path, and there is no way to skip past it: the text output
  keeps whatever it had already printed, while `--json` prints nothing at
  all, since the whole document is built before any of it is written.

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
