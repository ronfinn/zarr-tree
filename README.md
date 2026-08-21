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
├── 0 [array]
│   ├─ shape:  [2, 2048, 2048]
│   ├─ chunks: [1, 512, 512]
│   └─ dtype:  <u2
└── 1 [array]
    ├─ shape:  [2, 1024, 1024]
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

This is **recognition, not validation**. Nothing here checks a store against the
OME-NGFF specification: axes, datasets, coordinate transformations, `omero`
channels, labels and plate/well layouts are all ignored. For real validation use
the [OME-NGFF validator](https://ome.github.io/ome-ngff-validator/).

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
zarr-tree <directory>
```

Exactly one argument, a path to a local directory. Anything else prints a usage
line and exits with status 1:

```
$ zarr-tree
usage: zarr-tree <directory>
```

Two flags are recognised, each on its own:

```
    -h, --help       Print help
    -V, --version    Print version
```

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

The suite is in two parts: 9 unit tests in `src/main.rs`, which cover metadata
parsing directly, and 4 integration tests in `tests/cli.rs`, which run the
compiled binary against throwaway fixture stores and assert on what it prints.

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` on
every push and pull request.

## Limitations

- Local filesystem paths only. No remote or object-store access.
- Only `shape`, `chunks`/`chunk_shape` and `dtype`/`data_type` are read.
  Codecs, compressors, fill values, dimension names and user attributes are
  not shown.
- No output options: no depth limit, no filtering, no JSON output, no colour.
- V2 dtypes are passed through as stored and V3 dtypes given in object form
  (the extension syntax) are not interpreted.
- Sharding is not understood; a sharded array shows its declared chunk shape
  only.
- OME-Zarr support goes no further than spotting image groups and showing their
  version. No OME-NGFF validation, and no reading of axes, datasets, coordinate
  transformations, `omero`, labels or plate/well metadata.
- Symlinked directories are listed but not followed.

## Roadmap

Small, in roughly this order:

1. A `--depth` flag for large stores.
2. Show a node's user attributes when asked.
3. Report V3 dtypes given in object form, instead of showing them as missing.
4. Show an OME-Zarr image's axes alongside its version.

Remote stores and anything beyond lightweight OME-Zarr recognition are out of
scope for now.

## Why this project exists

It started as a Rust learning project. The goal was something small and real:
walk a directory, read a little JSON, and print it clearly — with each step
adding one idea (`Option` and `?`, borrowing, enums with data, unit tests)
rather than reaching for a framework.

It is also genuinely useful. Zarr stores are directory trees full of chunk
files, and `ls` or `tree` buries the structure in thousands of chunk keys.
`zarr-tree` shows the part that matters. The scope is deliberately small, and
the code is meant to stay readable.
