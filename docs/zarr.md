# Zarr reference

How `zarr-tree` reads plain Zarr: which files it opens, which fields it takes
out of them, what it prints, and where its interpretation deliberately stops.

This is not a tutorial on the Zarr specification. It documents one program's
reading of it. For the specification itself see
[zarr-specs.readthedocs.io](https://zarr-specs.readthedocs.io/).

- [Scope](#scope)
- [Zarr V2](#zarr-v2)
- [Zarr V3](#zarr-v3)
- [V2 and V3 side by side](#v2-and-v3-side-by-side)
- [Arrays and their metadata](#arrays-and-their-metadata)
- [Arrays are leaves](#arrays-are-leaves)
- [Sharding](#sharding)
- [Consolidated metadata](#consolidated-metadata)
- [Unknown and malformed nodes](#unknown-and-malformed-nodes)
- [Traversal boundaries](#traversal-boundaries)
- [JSON representation](#json-representation)
- [Validation checks](#validation-checks)
- [Deliberately not implemented](#deliberately-not-implemented)

## Scope

`zarr-tree` is a metadata inspector. It is not a Zarr implementation.

It reads enough metadata to identify each node in a store and describe its
structure: whether a node is a group or an array, and for an array its shape,
its chunk shape, its shard shape where it has one, and its dtype. That is the
whole of it.

It does not:

- decode array data, or read a chunk of any kind;
- execute or interpret codecs — the codec list is scanned for one name and
  otherwise left alone;
- resolve fill values, dimension names, compressors or user attributes;
- repair, rewrite or normalise a store;
- check a document against the Zarr specification.

Unfamiliar values are printed as stored rather than validated or translated.
The one deliberate exception is `--validate`, which checks what a store's own
metadata declares against what the store has — never a document against a
schema. See [Validation checks](#validation-checks).

The design rationale for all of this — why arrays are leaves, why a store is
five questions behind a trait, why consolidation is an overlay rather than a
second parser — is in [Architecture](architecture.md#design-principles).

## Zarr V2

Zarr V2 splits a node's metadata across separate files inside the node's own
directory. Which file is there answers what the node is, so classification
costs a read and no parsing at all.

| File | Read for |
| --- | --- |
| `.zgroup` | Node identification: this node is a group. Its contents are not used. |
| `.zarray` | Node identification and array metadata: `shape`, `chunks`, `dtype`. |
| `.zattrs` | User attributes. Read only on a group, and only to look for the OME-Zarr and SpatialData markers. |
| `.zmetadata` | Consolidated metadata at the store root — see [Consolidated metadata](#consolidated-metadata). |

`.zattrs` is read once per group and handed to both the OME-Zarr and the
SpatialData readers, which look at different keys and know nothing about each
other. On a remote store that saves a request per group; see
[Metadata classification](architecture.md#metadata-classification).

An array's fields come straight out of `.zarray`:

```
$ zarr-tree v2.zarr
v2.zarr [group]
└── measurements [array]
    ├─ shape:  [1024, 1024]
    ├─ chunks: [256, 256]
    └─ dtype:  <u2
```

**V2 dtypes are displayed exactly as stored**, in NumPy notation: `<u2`, `|u1`,
`<M8[ns]`. They are not translated into the V3 names (`uint16`, `uint8`), not
normalised for byte order, and not checked against the set of dtypes that
exist. A store that wrote something unrecognisable gets it printed back.

`.zarray` that is present but is not valid JSON still leaves the node an array
— the filename already answered that question — with every field showing `?`.
See [Unknown and malformed nodes](#unknown-and-malformed-nodes).

Zarr V2 has no sharding. There is one chunk grid, `chunks` is it, and no
`shards` row is ever drawn for a V2 array.

## Zarr V3

Zarr V3 uses one filename, `zarr.json`, for both node kinds and moves the
distinction inside the document. So a V3 node has to be parsed before it can be
classified.

| Key | Read for |
| --- | --- |
| `node_type` | Node identification: `"group"` or `"array"`. Any other value, or none, leaves the node `[unknown]`. |
| `shape` | An array's shape, copied as stored. |
| `chunk_grid.configuration.chunk_shape` | The chunk grid's shape. Only a `regular` grid records one; any other grid leaves `chunks` unreadable. |
| `codecs` | Scanned for a codec named exactly `sharding_indexed`, and for nothing else — see [Sharding](#sharding). |
| `data_type` | The dtype, when it is a string. |
| `attributes` | User attributes, read only on a group, and only for the OME-Zarr (`attributes.ome`) and SpatialData markers. |
| `consolidated_metadata` | Consolidated metadata at the store root — see [Consolidated metadata](#consolidated-metadata). |

Nothing else in a V3 document is read: not `chunk_key_encoding`, not
`fill_value`, not `dimension_names`, not `storage_transformers`, and not the
codec chain beyond the one name above.

A `data_type` given in the **object form** used by dtype extensions is not
interpreted. `data_type` is read as a string or not at all, so an extension
dtype shows as `?`. That is a
[roadmap item](roadmap.md#near-term), not a decision.

### V2 is checked first

Classification tries three files in a fixed order, and stops at the first
answer:

1. `.zgroup` — a V2 group.
2. `.zarray` — a V2 array.
3. `zarr.json` — a V3 group or array, according to `node_type`.

Nothing found: `[unknown]`.

**A directory carrying both V2 and V3 metadata is therefore reported as V2.**
This is worth stating because it is observable, not incidental: a store
mid-migration, or one written by a tool that emits both, takes the V2 reading —
including V2's dtype spelling and V2's `.zattrs` location for OME-Zarr and
SpatialData markers.

The order also costs something remotely. Every one of those three is an HTTP
`GET` against an object store, so a pure V3 store pays two misses per node
before it reaches `zarr.json`. See
[Remote efficiency](remote-stores.md#remote-efficiency).

## V2 and V3 side by side

Only rows this program actually reads:

| Concept | V2 | V3 |
| --- | --- | --- |
| Group marker | `.zgroup` exists | `zarr.json` with `"node_type": "group"` |
| Array marker | `.zarray` exists | `zarr.json` with `"node_type": "array"` |
| Attributes | `.zattrs`, a file of its own | `attributes` inside `zarr.json` |
| Shape | `shape` | `shape` |
| Chunk shape | `chunks` | `chunk_grid.configuration.chunk_shape`, or the sharding codec's `configuration.chunk_shape` when sharded |
| Shard shape | not applicable | `chunk_grid.configuration.chunk_shape` when sharded |
| dtype | `dtype`, NumPy notation | `data_type`, string form only |
| Consolidation | `.zmetadata` at the root | inline `consolidated_metadata` in the root `zarr.json` |
| OME-Zarr metadata | keys at the top level of `.zattrs` | `attributes.ome` |

## Arrays and their metadata

Every array prints three rows, and a sharded V3 array prints a fourth:

```
├─ shape:  [4096, 4096]
├─ chunks: [512, 512]
├─ shards: [2048, 2048]
└─ dtype:  uint16
```

`shape`, `chunks` and `dtype` are always drawn, showing `?` when the field
could not be read — they are the three things every Zarr array has, so their
absence is information. `shards` is drawn only for an array that named the
sharding codec; an unsharded array has no shards to be missing.

Dimension entries are copied out of the file rather than parsed into numbers.
A malformed `"shape": [1, "x"]` prints as `[1, "x"]` rather than being dropped
or repaired, and `--json` carries the same values as real JSON. Nothing is
multiplied out: no element count, no byte size, no chunk count is calculated,
because doing so would mean deciding what a non-numeric dimension meant.

## Arrays are leaves

Once a node is classified as an array, the walk stops there. No listing is
made below it, in the tree and in `--json` alike.

The consequences are worth spelling out:

- A V2 array's chunk keys (`0.0`, `0.1`, …) are never enumerated.
- A V3 array's `c/` chunk tree is never walked.
- A remote array holding millions of chunk objects costs exactly the reads that
  classified it, and no listing at all.
- `--depth` does not override this. Depth limits how far down the walk goes; it
  never pushes it further, and an array is already the end of its branch.

This is what makes a store you could not afford to `ls` inspectable at all, and
it is a structural property rather than a remembered rule: the one place a
listing is made is reached only for a group or an unknown node. See
[Walking the store](architecture.md#walking-the-store).

```
$ zarr-tree big.zarr
big.zarr [group]
└── volume [array]
    ├─ shape:  [8192, 8192, 8192]
    ├─ chunks: [64, 64, 64]
    └─ dtype:  uint8
```

That array is two million chunks on disk. The walk read one file.

## Sharding

A Zarr V3 array may pack many chunks into a single stored object — a *shard* —
using the `sharding_indexed` codec. When it does, the meaning of `chunk_grid`
changes: it no longer describes the chunks, it describes the shards, and the
chunk shape moves into the codec's own configuration.

| Metadata | Unsharded | Sharded |
| --- | --- | --- |
| `chunk_grid.configuration.chunk_shape` | the chunk shape | the **shard** shape |
| `sharding_indexed` codec's `configuration.chunk_shape` | absent | the **chunk** shape |

`zarr-tree` reads both and prints them under the names that match what they
are:

```
$ zarr-tree sharded.zarr
sharded.zarr [group]
└── img [array]
    ├─ shape:  [4096, 4096]
    ├─ chunks: [512, 512]
    ├─ shards: [2048, 2048]
    └─ dtype:  uint16
```

That array is 4096×4096, stored as 2048×2048 shards, each holding sixteen
512×512 chunks.

The codec is found by its exact name in the `codecs` list, never guessed at
from the shapes themselves — two grids of different sizes are not evidence of
sharding, and a store is free to have any chunk shape it likes.

**Which shape is which is decided by whether the codec is there, not by whether
its inner shape could be read.** A `sharding_indexed` codec whose
`configuration.chunk_shape` is missing or unreadable leaves `chunks` as `?`:

```
$ zarr-tree badshard.zarr
badshard.zarr [group]
└── img [array]
    ├─ shape:  [4096, 4096]
    ├─ chunks: ?
    ├─ shards: [2048, 2048]
    └─ dtype:  uint16
```

Falling back to the grid shape there would print a shard under the name
`chunks`, which is the one mistake this branch exists to prevent. A wrong
number is worse than a `?`.

Nothing else about the sharding is read: not the index location, not the index
codecs, not the inner codec chain, not the shard layout in storage. The codec
is never executed.

## Consolidated metadata

Walking a store means one small read per node plus one listing per group.
Consolidation replaces all of that with a single document at the store root
holding a copy of every metadata file in the tree.

Two forms are read — the two current zarr-python writes.

**Zarr V2** keeps it in `.zmetadata`, a flat map keyed by the very paths a walk
would have read:

```json
{
  "zarr_consolidated_format": 1,
  "metadata": {
    ".zgroup":            {"zarr_format": 2},
    ".zattrs":            {"note": "root"},
    "images/.zgroup":     {"zarr_format": 2},
    "images/0/.zarray":   {"shape": [64, 64], "chunks": [32, 32], "dtype": "|u1"}
  }
}
```

The keys are what give the hierarchy: `images/0/.zarray` says there is an array
at `images/0`, and by saying so says there is a node at `images` too. Only the
metadata filenames are recognised — `.zgroup`, `.zarray`, `.zattrs`,
`zarr.json` — so nothing else in the map can become a node, and a chunk key
never does. `zarr_consolidated_format` 1 is the only version there has been;
any other value is left alone.

**Zarr V3** keeps it inside the root `zarr.json`, as a `consolidated_metadata`
block whose entries are whole documents keyed by path:

```json
{
  "zarr_format": 3,
  "node_type": "group",
  "consolidated_metadata": {
    "kind": "inline",
    "must_understand": false,
    "metadata": {
      "images":   {"zarr_format": 3, "node_type": "group", "attributes": {}},
      "images/0": {"zarr_format": 3, "node_type": "array", "shape": [64, 64]}
    }
  }
}
```

`kind: "inline"` — the metadata is in the block itself — is the only kind read.
`must_understand: false` says a reader that does not understand the block may
ignore it, which is exactly what `zarr-tree` does with anything else. A group's
block is defined to hold that group's own children, so a non-empty nested block
is followed by the same rule; zarr-python writes the flat form above and leaves
nested blocks empty.

V3 consolidation is younger and less settled than V2's — zarr-python warns that
it is not part of the Zarr V3 specification and may change — so only the form
it writes today is read.

Two properties matter more than the formats:

**It is opportunistic.** A store with no consolidated metadata, or with a form
not read here, is walked exactly as it would have been otherwise. Nothing that
worked without consolidation comes to depend on it.

**It is all-or-nothing.** Once the document has been read it is the only thing
read: `.zmetadata` is tried first, then the root `zarr.json`, and after that
the store is not consulted again for any Zarr metadata or any listing. A
consolidated document is a snapshot, taken when somebody last called
`zarr.consolidate_metadata`, and it may be stale. A tree that took some nodes
from the snapshot and some from live reads would show two moments at once and
mark neither. So a stale snapshot is reported as it stands, unchecked — and
checking it would cost exactly the requests consolidation exists to avoid.

The exception is a binary payload. A SpatialData element's Parquet file is not
Zarr, appears in no consolidated document, and is read where it lies; the
physical store is kept for that alone. See
[Consolidated metadata](architecture.md#consolidated-metadata) for how the
overlay is built.

This is what makes a plain static HTTP server — which can never answer the
`PROPFIND` a listing needs — walkable in full. See
[Static HTTP and consolidated metadata](remote-stores.md#static-http-and-consolidated-metadata).

## Unknown and malformed nodes

Metadata that is missing, unreadable or malformed costs that one node's label
or one field, and never the rest of the walk.

| Situation | Result |
| --- | --- |
| No metadata file this program reads | `[unknown]`, and the node is still descended into |
| `zarr.json` present but not valid JSON | `[unknown]` |
| `zarr.json` with an unrecognised `node_type` | `[unknown]` |
| `.zarray` present but not valid JSON | `[array]` with every field `?` |
| A field missing, of the wrong type, or unreadable | `?` in the tree, `null` in `--json` |
| `.zattrs` missing or unparseable | The group simply gets no OME-Zarr or SpatialData tag |

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

`good` is a well-formed V3 array; `plain` is a V2 array whose `.zarray` omits
`chunks` and `dtype`; `truncated` has a `zarr.json` that is not valid JSON.

This is deliberately different from a **store access error**. A parse failure
is data the program could not make sense of, and the walk continues. A
directory that cannot be listed at all is the storage layer refusing to answer,
and that ends the walk with a message on stderr and exit status 1 — the text
output keeps whatever it had already printed, while `--json` prints nothing,
because the whole document is built before any of it is written. See
[Error and degradation model](architecture.md#error-and-degradation-model).

On a remote store the distinction is weaker by necessity: a read that failed
mid-walk is indistinguishable from a file that is not there, so such a node is
reported `[unknown]`. Only the root is checked properly, before anything is
printed.

## Traversal boundaries

Besides stopping at arrays, the walk observes four rules:

- **Unknown nodes are descended into.** A directory whose metadata could not be
  read may still have recognisable nodes below it, and losing a subtree to one
  bad file would be the wrong trade.
- **Only directories are children.** Locally that means entries the filesystem
  reports as real directories; remotely it means the common prefixes of a
  listing, so a node's own metadata objects and any loose files beside them are
  not children. Symlinks are not followed, and a symlinked directory is not
  listed at all — which is also what stops a link pointing back at an ancestor
  from looping.
- **Children are sorted lexicographically**, so a run against the same store
  prints the same tree. `10` therefore sorts before `9`; natural ordering for
  numbered names is [under consideration](roadmap.md#under-consideration).
- **`--depth N` limits how far below the root the walk goes.** At the limit the
  directory is not read at all, which is what makes `--depth 0` cheap on a
  store with a million chunk files. A node that is shown keeps its own metadata
  rows, because those describe the node itself rather than anything below it.
  See [Depth](getting-started.md#depth).

## JSON representation

`--json` is a second renderer over one reading of the metadata, not a second
interpretation, so the tree and the document cannot disagree about what a store
contains.

Every node carries `name`, `kind` and `children`; an array additionally carries
an `array` object. (Nodes also carry `ome`, `spatialdata`, `parquet` and
`anndata` sections where those apply — see
[OME-Zarr reference](ome-zarr.md#json-representation) and the SpatialData
material in the [README](../README.md#json).)

| Field | Meaning |
| --- | --- |
| `name` | The directory name. On the root, the path as it was typed. |
| `kind` | `"group"`, `"array"` or `"unknown"` |
| `children` | The child nodes, in the order the tree lists them. Empty for an array, and empty at the depth limit. |
| `array` | `shape`, `chunks`, `dtype`, and `shards` on a sharded V3 array. |

```
$ zarr-tree --json sharded.zarr | jq '.children[0]'
{
  "array": {
    "chunks": [512, 512],
    "dtype": "uint16",
    "shape": [4096, 4096],
    "shards": [2048, 2048]
  },
  "children": [],
  "kind": "array",
  "name": "img"
}
```

Two rules govern the `array` object:

- A field that was looked for and could not be read is `null`, the same thing
  the tree draws as `?`. `shape`, `chunks` and `dtype` are therefore always
  present.
- `shards` is **omitted entirely** on an unsharded array rather than written as
  `null`. The two say different things: `null` means "looked for, not
  readable", and an unsharded array has no shards to miss.

Object keys come out in alphabetical order — `serde_json`'s default, kept
rather than fought.

## Validation checks

`--validate` was merged **after v0.3.0** and is available by building `master`.
It is not in any release yet.

It prints findings instead of the tree, over the same metadata the tree reads.
Two of the seven rules are about plain Zarr; the rest are OME-Zarr and
SpatialData and are documented in
[OME-Zarr reference](ome-zarr.md#validation) and the
[README](../README.md#validation).

**Node identification.** Every node the walk entered could be identified.

| Finding | Severity |
| --- | --- |
| The store root has no Zarr metadata this tool can read | `ERROR` |
| A node below the root has none | `WARN` |
| The root was readable | `PASS` — `Zarr root metadata is readable` |

A directory further down that says nothing is a directory somebody put there —
a `.git`, a stray export — and a warning is as much as can honestly be made of
it.

**Array dimensionality.** An array's own grids have to agree with its shape.

| Finding | Severity |
| --- | --- |
| No readable `shape` | `ERROR` — every Zarr array has one |
| `chunks` has the same number of dimensions as `shape` | `PASS` |
| `chunks` disagrees with `shape` | `ERROR` |
| No readable chunk shape | `WARN` — only a regular grid records one, so nothing is claimed |
| `shards` agrees with `shape` (sharded arrays only) | `PASS` |
| `shards` disagrees with `shape` | `ERROR` |

```
$ zarr-tree --validate sharded.zarr
PASS  /  Zarr root metadata is readable
PASS  /img  array shape and chunks agree on 2 dimensions
PASS  /img  array shape and shards agree on 2 dimensions

Validation: 3 passed, 0 warnings, 0 errors
```

An array whose sharding codec could not be read through warns rather than
errors, because the check could not be made:

```
$ zarr-tree --validate badshard.zarr
PASS  /  Zarr root metadata is readable
WARN  /img  array declares no readable chunk shape, so its dimensions were not checked
PASS  /img  array shape and shards agree on 2 dimensions

Validation: 2 passed, 1 warning, 0 errors
```

`WARN` always means "could not be checked" and never "broken". Codecs, fill
values and dtypes are not checked at all: this is a dimensionality check, not a
Zarr conformance pass.

`--validate` walks the store whole, so it is refused together with `--depth`.
Exit status is 0 when nothing worse than a warning was found and 2 when at
least one `ERROR` was — see [Validation](getting-started.md#validation).

## Deliberately not implemented

These are boundaries, not gaps. Several of them are what make the walk cheap
enough to run against a remote store at all.

- **Chunk reads of any kind**, and no chunk object is ever listed.
- **Decompression and codec execution.** The `codecs` list is scanned for one
  name; nothing in it is run or interpreted.
- **Data-value inspection.** No element of any array is read, so nothing is
  counted, summed or ranged.
- **Complete Zarr specification validation.** `--validate` checks a store
  against its own declarations, never a document against a schema.
- **Store repair, rewriting or writing of any kind.**
- **Compressors, fill values, dimension names and user attributes.** Attributes
  are read only for the OME-Zarr and SpatialData markers, and are never
  displayed as attributes.
- **dtype translation.** V2 dtypes are passed through in NumPy notation and are
  never mapped onto V3 names. V3 dtypes in object (extension) form are not
  interpreted and show as `?` — that one is on the
  [roadmap](roadmap.md#near-term).
- **Consolidation forms beyond the two above**, and checking a consolidated
  document against the store it describes.

Storage backends are a separate boundary — local, S3 and HTTP(S) are the whole
list, with no ZIP store, GCS or Azure, and no scraping of HTML directory-index
pages. The full matrix, including what is supported in v0.3.0 versus on
`master`, is in [Project status](status.md#zarr).

## See also

- [Getting started](getting-started.md) — building it, the option set, and exit
  status.
- [OME-Zarr reference](ome-zarr.md) — the metadata layered on top of these
  groups.
- [SpatialData reference](spatialdata.md) — the conventions layered on top of
  those, and the two payload formats they use.
- [Remote stores](remote-stores.md) — S3, HTTP, WebDAV, and static HTTP via
  consolidated metadata.
- [Architecture](architecture.md) — the `Store` trait, the consolidated
  overlay, and how classification and validation fit together.
- [Project status](status.md#zarr) — the capability matrix.
- [Roadmap](roadmap.md) — direction, with nothing promised.
