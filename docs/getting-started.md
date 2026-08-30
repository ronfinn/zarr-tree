# Getting started

`zarr-tree` walks a Zarr store, reads the small JSON metadata documents
scattered through it, and prints the hierarchy as a tree. This page takes you
from a fresh clone to a first inspected store. For the full reference, see the
[README](../README.md); for remote stores, see [Remote stores](remote-stores.md).

## Requirements

- Rust 1.88 or newer.

Edition 2024 itself needs only 1.85; 1.88 is the effective minimum imposed by
the current `object_store` release, which uses let-chains. Development happens
on rustc 1.98.0.

If you have no toolchain, [rustup](https://www.rust-lang.org/tools/install) is
the recommended way to install one.

Nothing else is required. There is no runtime dependency, no configuration file
and no service to sign in to.

## Install from source

`zarr-tree` is not published on crates.io and there are no pre-built release
binaries yet, so building from source is the only installation path.

```sh
git clone https://github.com/ronfinn/zarr-tree.git
cd zarr-tree
cargo build --release
```

The binary lands at `target/release/zarr-tree`:

```
$ ./target/release/zarr-tree --version
zarr-tree 0.3.0
```

To put it on your `PATH`, install it into Cargo's bin directory
(`~/.cargo/bin` by default):

```sh
cargo install --path .
```

The examples below assume `zarr-tree` is on your `PATH`.

### Release versus master

The latest tagged release is **v0.3.0**; `master` may contain unreleased
features. The manifest version stays at `0.3.0` until the next release is cut,
so a build from `master` also reports `zarr-tree 0.3.0` while carrying work that
is not in the v0.3.0 release. The one user-visible difference today is
`--validate`, which was merged after v0.3.0.

[Project status](status.md) records which capabilities are released and which
are `master`-only.

## Your first store

Point it at a store directory:

```
$ zarr-tree example.zarr
example.zarr [group]
├── labels [group]
│   └── cells [array]
│       ├─ shape:  [1024, 1024]
│       ├─ chunks: [512, 512]
│       └─ dtype:  uint8
└── measurements [array]
    ├─ shape:  [1024, 1024]
    ├─ chunks: [256, 256]
    └─ dtype:  uint16
```

Every node carries a label saying what its metadata identified it as:

| Label | Meaning |
| --- | --- |
| `[group]` | A Zarr group — a `.zgroup` (V2) or a `zarr.json` of `node_type` `group` (V3). |
| `[array]` | A Zarr array. Arrays are leaves: the walk stops here. |
| `[unknown]` | A directory whose metadata is missing or could not be parsed. It is still descended into. |

An array prints the three fields read from its metadata — `shape`, `chunks` and
`dtype` — and, on a sharded Zarr V3 array, a fourth `shards` row giving the
chunk-grid shape while `chunks` gives the inner chunk shape. A field that could
not be read prints as `?` rather than stopping the walk.

Groups recognised as OME-Zarr or SpatialData pick up an extra tag and a few
metadata rows: `[group, OME-Zarr 0.4]`, `[group, SpatialData points]`. The
README covers those in full.

## Common commands

| Goal | Command |
| --- | --- |
| Inspect a store | `zarr-tree STORE` |
| Limit traversal | `zarr-tree --depth 2 STORE` |
| JSON output | `zarr-tree --json STORE` |
| Validate metadata | `zarr-tree --validate STORE` |
| Validation as JSON | `zarr-tree --validate --json STORE` |
| Help | `zarr-tree --help` |
| Version | `zarr-tree --version` |

`--validate` is available on development `master` after v0.3.0. Everything else
above is in the v0.3.0 release.

`STORE` is a directory on this machine, an `s3://` URI or an `http(s)://` URL.
All three take the same walk and accept the same options — see
[Remote stores](remote-stores.md).

## Depth

`--depth N` limits how far below the root the walk goes.

```
$ zarr-tree --depth 0 example.zarr
example.zarr [group]

$ zarr-tree --depth 1 example.zarr
example.zarr [group]
├── labels [group]
└── measurements [array]
    ├─ shape:  [1024, 1024]
    ├─ chunks: [256, 256]
    └─ dtype:  uint16
```

A node shown at the limit keeps its own metadata rows — those describe the node,
not anything below it — but its children are not read at all, which is what makes
`--depth 0` cheap on a store with a million chunk files.

Arrays are leaves at any depth. The limit never has anything to say about them,
because the walk already stops there, and an array's chunk objects are never
listed or fetched.

`--depth` cannot be combined with `--validate`. See
[Validation](../README.md#validation) for why.

## JSON

`--json` prints the same walk as one JSON document, for `jq` and scripts. It
combines with `--depth`.

```
$ zarr-tree --json example.zarr | jq '.children[].name'
"labels"
"measurements"
```

Every node has `name`, `kind` and `children`, plus one section per kind of
metadata that applies to it — `array`, `ome`, `spatialdata`, `parquet`,
`anndata`. A section is absent when that metadata does not apply, and a field
inside a section is `null` when the file gave no readable value, which is the
same rule the tree follows when it prints `?`. The full field table is in the
README's [JSON](../README.md#json) section.

## Validation

`--validate` checks what a store's metadata *declares* against what the store
*has*, and prints findings instead of the tree. It reads metadata only. This is
a metadata-only structural check, not Zarr, OME-NGFF or SpatialData
specification conformance.

```
$ zarr-tree --validate example.zarr
PASS  /  Zarr root metadata is readable
PASS  /labels/cells  array shape and chunks agree on 2 dimensions
PASS  /measurements  array shape and chunks agree on 2 dimensions

Validation: 3 passed, 0 warnings, 0 errors
```

A store that declares something it does not have reports it and exits 2:

```
$ zarr-tree --validate broken.zarr
PASS  /  Zarr root metadata is readable
PASS  /image  OME dataset path "0" exists
ERROR /image  OME dataset path "1" does not exist
PASS  /image  pyramid levels agree with the multiscale's axes on 2 dimensions
PASS  /image/0  array shape and chunks agree on 2 dimensions

Validation: 4 passed, 0 warnings, 1 error
```

The three severities mean three different things: `PASS` — the declared
structure is there; `WARN` — the check could not be made, and nothing is claimed
either way; `ERROR` — the metadata declares something the store does not have.

Exit status:

| Status | Meaning |
| --- | --- |
| 0 | The store was walked. With `--validate`, nothing worse than a `WARN`. |
| 1 | The store could not be read, or the command line made no sense. |
| 2 | `--validate` ran and reported at least one `ERROR`. |

The seven rules are listed under [Validation](../README.md#validation).

## Next steps

- [Remote stores](remote-stores.md) — S3, HTTP, WebDAV, and static HTTP via
  consolidated metadata.
- [Project status](status.md) — the capability matrix, and what is deliberately
  absent.
- [Roadmap](roadmap.md) — direction, with nothing promised.
- [README](../README.md) — the full reference, including OME-Zarr, SpatialData
  and the complete list of limitations.
