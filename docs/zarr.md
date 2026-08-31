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
- [Format version](#format-version)
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
- execute or interpret codecs — a codec chain is listed by name and nothing in
  it is run, instantiated or configured;
- resolve fill values, codecs or user attributes — a fill value, a codec name
  and a V3 array's `dimension_names` are displayed as stored, never resolved,
  converted or filled in;
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
| `.zarray` | Node identification and array metadata: `shape`, `chunks`, `dtype`, `fill_value`, `filters`, `compressor`. |
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
├─ zarr: V2
└── measurements [array]
    ├─ zarr:   V2
    ├─ shape:  [1024, 1024]
    ├─ chunks: [256, 256]
    ├─ dtype:  <u2
    ├─ fill:   0
    └─ codecs: blosc
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
| `codecs` | The declared codec chain, by name — see [Codecs](#codecs) — and scanned for a codec named exactly `sharding_indexed` — see [Sharding](#sharding). |
| `data_type` | The dtype: the string itself, or the `name` of an extension's object form. |
| `dimension_names` | The array's own dimension names — see [Dimension names](#dimension-names). |
| `fill_value` | The array's fill value, as stored — see [Fill values](#fill-values). |
| `attributes` | User attributes, read only on a group, and only for the OME-Zarr (`attributes.ome`) and SpatialData markers. |
| `consolidated_metadata` | Consolidated metadata at the store root — see [Consolidated metadata](#consolidated-metadata). |

Nothing else in a V3 document is read: not `chunk_key_encoding`, not
`storage_transformers`, and no codec's `configuration`.

### Dimension names

A V3 array may name its own dimensions. The key is optional, and where it
appears it is a list as long as the shape whose entries are each a string or
`null`:

```json
"dimension_names": ["c", null, "y", "x"]
```

The names are shown in order on a `dimensions` row, after `dtype`:

```
└─ dimensions: c, ?, y, x
```

A `null` entry means the dimension is there and is deliberately unnamed. It
keeps its position as `?` rather than being dropped, because dropping it would
slide every later name onto the wrong dimension, and no name is invented to
fill the gap — not from the shape, not from a convention about what four
dimensions are usually called. An entry that is neither a string nor `null`
gets the same `?`.

An array that declares no `dimension_names`, or whose key is not a list, prints
no row at all and carries no `dimension_names` in `--json`; where the key is
there, `--json` gives back the list as stored, `null` entries included. A
malformed key costs that one row and nothing else: the array is still an array
and the walk goes on.

Zarr V2 has no equivalent key, and none is invented for it. A V2 array's output
is exactly what it has always been.

These are the *array's* dimension names, and they are not the same thing as
OME-Zarr `axes`. The axes are NGFF semantic metadata, read from a multiscale
group's attributes and describing what the dimensions of that image *mean*; the
dimension names are plain Zarr metadata belonging to one array. An array under
an OME-Zarr image may carry both, in which case both are shown on their own
rows — neither is derived from the other, and neither replaces the other. See
[OME-Zarr](ome-zarr.md).

### Object-form data types

V3 spells a data type in two ways, and both are read. A core type is a bare
string:

```json
"data_type": "uint16"
```

An extension type is an object naming the extension, usually with a
configuration beside it:

```json
"data_type": {"name": "numpy.datetime64", "configuration": {"unit": "s"}}
```

Only the `name` is displayed, so that array shows `dtype: numpy.datetime64`.
The configuration is what a *reader* needs in order to decode values, and this
tool decodes nothing; the full object stays in the file. The name is passed
through exactly as stored and is not checked against any registry, so an
extension this tool has never heard of is displayed rather than judged — the
same treatment a V2 dtype in NumPy notation gets.

A `data_type` with no name to show — an object without a `name`, a `name` that
is not a string, or a value that is neither string nor object — leaves the
dtype unread and the row shows `?`. That costs the reader one row and nothing
else: the node is still an array, and the walk continues.

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
| dtype | `dtype`, NumPy notation | `data_type`, string form or an extension object's `name` |
| Fill value | `fill_value`, required, may be `null` | `fill_value` |
| Codec chain | `filters` in order, then `compressor`; each by its `id` | `codecs` in order, each by its `name` |
| Consolidation | `.zmetadata` at the root | inline `consolidated_metadata` in the root `zarr.json` |
| OME-Zarr metadata | keys at the top level of `.zattrs` | `attributes.ome` |

## Format version

Every node `zarr-tree` recognised carries a `zarr` row saying which of the two
metadata formats it was read as — the first row under the node's own line, on
groups and arrays alike:

```
$ zarr-tree mixed.zarr
mixed.zarr [group]
├─ zarr: V3
├── img [array]
│   ├─ zarr:   V3
│   ├─ shape:  [64, 64]
│   ├─ chunks: [32, 32]
│   └─ dtype:  uint16
└── legacy [group]
    ├─ zarr: V2
    └── mask [array]
        ├─ zarr:   V2
        ├─ shape:  [8, 8]
        ├─ chunks: [4, 4]
        └─ dtype:  |u1
```

A store is not required to be all one version, which is why the row is per node
rather than per store: a V3 root can hold a V2 subtree, and the tree above says
so node by node.

The row reports the format **this program actually read the node as**, not
which metadata files happen to sit in its directory. Those are the same thing
almost everywhere, and differ in exactly one case:

> A node carrying both layouts — a `.zgroup`/`.zarray` *and* a `zarr.json` —
> reports **V2**. Classification checks V2 first, so V2 is the document every
> other field on that node was read out of, and the row has to describe that
> reading. The `zarr.json` beside it was never opened.

An `[unknown]` node gets **no row at all**. Nothing classified it, so no
metadata document was believed and there is no version to report — and a
version guessed from a filename that failed to parse is the one thing this row
must never be. `--json` leaves the key out entirely for the same reason; see
[JSON representation](#json-representation).

The row costs no extra read anywhere. Which branch of the classification
answered is already known by the time a node is drawn, so nothing is re-opened
to render it.

This is a layer below any semantic tag. An OME-Zarr image or a SpatialData
element still carries its own label, and gains the `zarr` row beside it:

```
image.zarr [group, OME-Zarr 0.5]
├─ zarr: V3
├─ axes: c, y, x
├─ pyramid levels: 3
└─ datasets: 0, 1, 2
```

`OME-Zarr 0.5` is what the attributes mean; `V3` is the metadata format they
were stored in. Neither is derived from the other, and the two version numbers
move independently — OME-Zarr 0.4 stores are V2, OME-Zarr 0.5 stores are V3,
but that is a fact about those specifications, not a rule this program applies.

## User attributes

Both versions let a node carry arbitrary user attributes, in the two places the
table above names: a `.zattrs` file of its own in V2, an `attributes` member of
`zarr.json` in V3. They are not shown by default. `--attributes` shows them,
under one row name for both versions:

```
$ zarr-tree --attributes example.zarr
example.zarr [group]
├─ zarr: V3
├─ attributes: {"batch":3,"experiment":"A"}
└── img [array]
    ├─ zarr:       V3
    ├─ shape:      [64, 64]
    ├─ chunks:     [32, 32]
    ├─ dtype:      |u1
    └─ attributes: {"unit":"nm"}
```

The object is printed exactly as stored, as compact JSON on one line, with keys
sorted so the same store always prints the same text. Nothing in it is
interpreted: no key becomes a row of its own, because an arbitrary user key is
not something this program understands and dressing it up as one would say
otherwise. `--json` carries the same object with its values' types intact.

Attributes this program *does* read — `multiscales`, `ome`,
`spatialdata_attrs`, `encoding-type` — are not filtered out of the raw object.
The semantic rows above it are this tool's reading; the attributes row is the
document that reading came from, and seeing both is the point.

| The node's attributes | Tree | `--json` |
| --- | --- | --- |
| Absent, or an empty object | no row | no key |
| A non-empty object | `attributes: {…}` | the object, values intact |
| Unreadable — a `.zattrs` that is not JSON, or an `attributes` member that is not an object | `attributes: ?` | `null` |

An empty `{}` earns no row: zarr-python writes one into every node it creates,
so a row on all of them would bury the nodes that say something. An unreadable
document is kept distinct from an absent one — the `?`/`null` rule the rest of
this reference describes for [malformed
metadata](#unknown-and-malformed-nodes) — so a bad `.zattrs` is never passed
off as a node with nothing to say, and never stops the walk.

One note on cost: a **V2 array** keeps its attributes in a file a default walk
never opens, so `--attributes` costs one extra read per V2 array. V2 groups and
all V3 nodes cost nothing extra, their attributes being in a file already read.

## Arrays and their metadata

Every array prints its format and then three rows. A sharded V3 array prints a
`shards` row after its chunks, an array that declares a fill value prints a
`fill` row after its dtype, a V3 array that names its dimensions prints a
`dimensions` row after that, an array declaring a codec chain prints a `codecs`
row, and an array whose document says anything about chunk order or chunk
naming prints a `layout` row last:

```
├─ zarr:       V3
├─ shape:      [3, 4096, 4096]
├─ chunks:     [1, 512, 512]
├─ shards:     [1, 2048, 2048]
├─ dtype:      uint16
├─ fill:       0
├─ dimensions: c, y, x
├─ codecs:     sharding_indexed
└─ layout:     encoding=default, separator="/"
```

`shape`, `chunks` and `dtype` are always drawn, showing `?` when the field
could not be read — they are the three things every Zarr array has, so their
absence is information. `shards` is drawn only for an array that named the
sharding codec; an unsharded array has no shards to be missing. `fill` is drawn
only for an array whose document has a `fill_value` key — see
[Fill values](#fill-values). `dimensions` is drawn only for a V3 array that
declared `dimension_names` — see [Dimension names](#dimension-names). `codecs` is drawn only for an array
declaring a chain — see [Codecs](#codecs). `layout` is drawn last, and only
where the document said something about it — see
[Chunk layout](#chunk-layout). The row names are padded to the longest one actually
printed, so an array with no `dimensions` row is laid out exactly as it always
was.

Dimension entries are copied out of the file rather than parsed into numbers.
A malformed `"shape": [1, "x"]` prints as `[1, "x"]` rather than being dropped
or repaired, and `--json` carries the same values as real JSON. Nothing is
multiplied out: no element count, no byte size, no chunk count is calculated,
because doing so would mean deciding what a non-numeric dimension meant.

## Fill values

Both Zarr versions let an array declare the value a chunk that was never
written stands for. V2 puts `fill_value` in `.zarray`, V3 puts it in
`zarr.json`, and both are read the same way: copied out of the document
untouched.

The value has no single JSON type. All of these are things real stores write:

```json
"fill_value": 0
"fill_value": -1
"fill_value": 3.14
"fill_value": "NaN"
"fill_value": null
"fill_value": [0.0, 1.0]
```

So it is shown as compact JSON, which is what makes the type visible:

```
├─ dtype:  <f4
└─ fill:   "NaN"
```

The quotes are the point. `"NaN"` is how both versions spell a not-a-number
fill for a float array, and it is stored as the three characters `NaN` in a
string. Turning it into a floating-point NaN would be decoding the value, which
is the line this tool does not cross — so it stays a string, and `"Infinity"`
and `"-Infinity"` with it. Nothing here is special-cased.

Nothing is checked against the dtype either. A `<u2` array declaring
`"fill_value": -1` prints `fill: -1`, because reporting what a store says is
this tool's job and deciding whether a store is wrong about its own fill value
would need the dtype semantics it deliberately does not have. `--validate` does
not look at fill values.

**A stated `null` is not a missing key.** V2 requires `fill_value` and spells
"this array declares no fill value" as `null`, which is a fact the document
states:

```
└─ fill:   null
```

An array whose document has no `fill_value` at all stated nothing, and gets no
row and no JSON key. No default is invented for it — not `0`, not `null`. The
same rule as `shards` and `dimension_names`: a key that is not applicable does
not appear, and the difference between the two absences is preserved rather
than flattened.

In `--json` the value is the JSON value the document held, in its own type:

```json
"array": {
  "shape": [4],
  "chunks": [4],
  "dtype": "<f4",
  "fill_value": "NaN"
}
```

Both documents were already being parsed for `shape` and `dtype`, so reading
the fill value costs no extra request anywhere — local, S3, HTTP or a
consolidated snapshot alike.

## Codecs

Both Zarr versions let an array declare what happens to a chunk between the
stored bytes and the values — compression, byte order, a filter or two — and
both spell it differently. One row answers the question for both:

```
└─ codecs: delta, blosc
```

**Names only, in declaration order.** Every codec carries a configuration
beside its name — blosc's `cname` and `clevel`, gzip's `level`, delta's
`dtype` — and none of it is read or shown. That is the same boundary an
extension dtype's `configuration` sits behind: configuration is what a *reader*
needs in order to decode, and nothing here decodes. Names are not checked
against any registry either, so an extension codec is printed as stored:

```
└─ codecs: bytes, numcodecs.zfpy
```

The order is the metadata and is never sorted. `delta, blosc` and
`blosc, delta` are different pipelines, so the chain is shown exactly as the
document declares it — unlike child names, which are ordered for reading.

### V2: filters, then compressor

V2 splits the chain across two keys of `.zarray`. `filters` is a list applied
to the values first, and `compressor` is the single codec applied last:

```json
"filters": [{"id": "delta", "dtype": "<i4"}],
"compressor": {"id": "blosc", "cname": "lz4", "clevel": 5}
```

Each contributes its `id`, in the order they run, giving `codecs: delta, blosc`.
The row does not say which of the two keys an entry came from, because the
question it answers is *what runs, in what order* — and the answer reads the
same whichever version wrote the store.

**An array declaring no processing prints no row.** `"filters": null` with
`"compressor": null` is how V2 spells an array stored raw; there is nothing to
report, so nothing is reported, and no `codecs: none` is invented. This follows
the rule `shards` and `dimension_names` already do — a field with nothing to
report does not report. A V3 array is rarely quiet in the same way: its
`codecs` is required, and even an uncompressed array declares
`[{"name": "bytes"}]`.

### V3: the codecs list

V3 keeps one list in `zarr.json`, each entry an object naming a codec:

```json
"codecs": [{"name": "bytes", "configuration": {"endian": "little"}},
           {"name": "blosc", "configuration": {"cname": "zstd"}}]
```

which reads as `codecs: bytes, blosc`.

### A sharded array shows one codec

```
├─ chunks: [64, 64]
├─ shards: [256, 256]
├─ dtype:  uint16
└─ codecs: sharding_indexed
```

This is the document's `codecs` key, and for a sharded array that key really is
that one entry. The codecs applied to the chunks *inside* a shard live in
`sharding_indexed`'s `configuration`, which is not displayed — the same
boundary every other codec's configuration sits behind, and what keeps a
summary row from growing into a codec tree. What the sharding does to the grid
is already reported, and more usefully, as the two shape rows above it; see
[Sharding](#sharding).

### Unreadable entries keep their place

A codec whose name cannot be read — an entry with no `id` or `name`, a name
that is not a string, an entry that is not an object — becomes `?` in its own
position rather than being dropped:

```
└─ codecs: bytes, ?, blosc
```

Dropping it would show a two-codec chain where the document declares three,
which is a stronger claim than "we could not name this one". In `--json` that
position is `null`, the convention `dimension_names` set:

```json
"codecs": ["bytes", null, "blosc"]
```

A whole field with no positions to hold — a V3 `codecs` that is not a list or
is empty, a V2 `filters` that is not a list — contributes nothing, and an array
left with no chain at all gets no row and no JSON key.

Both keys are in documents already parsed for `shape` and `dtype`, so the
chain costs no extra request anywhere.

## Chunk layout

Both Zarr versions let an array say something about how its chunks are ordered
and how they are named, and neither says it the same way. One row answers the
question for both:

```
└─ layout: order=C, separator="."
```
```
└─ layout: encoding=default, separator="/"
```

The first is a V2 array, the second a V3 one. **The label is shared; the model
is not.** Each half is named because the two are unrelated facts sharing a row
for brevity — how a chunk's values run in memory is not what a chunk's object
key looks like — and a bare `C, "."` would read as though they were one thing.
The row groups them so the question *how is this array laid out, and how are
its chunks keyed?* can be asked without first knowing which version wrote the
store; it does not claim the two versions model the same thing.

The separator keeps its quotes for the same reason a fill value keeps its JSON
spelling: `separator=.` ends a row in what looks like a full stop, and a
`separator=""` — a value a document may really hold — would otherwise show
nothing at all.

**Nothing is checked and nothing is built.** An `order` is not required to be
`C` or `F` here, a separator is not required to be `.` or `/`, and an encoding
name is not looked up in any registry. No chunk key is ever constructed,
guessed at or looked for — that would be a chunk read, which is the boundary
[Arrays are leaves](#arrays-are-leaves) draws. This row reports what the
document says about chunk naming; it never goes to see.

### V2: order and dimension_separator

V2 puts two independent keys in `.zarray`:

```json
"order": "C",
"dimension_separator": "/"
```

`order` says whether a chunk's values run C-first or Fortran-first.
`dimension_separator` says what goes between the indices in a chunk's name, so
that chunk `(0, 1)` is stored as `0.1` or as `0/1`. Together they read as
`layout: order=C, separator="/"`.

### V3: chunk_key_encoding

V3 has neither key. It has one object naming the scheme that turns a chunk's
position in the grid into an object key, and configuring it:

```json
"chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}}
```

which reads as `layout: encoding=default, separator="/"`. The name is shown
exactly as stored, so an extension encoding is displayed rather than judged —
the rule an extension dtype and an extension codec name already follow:

```
└─ layout: encoding=my.chunk.encoding, separator="-"
```

**Only the `separator` is read out of the `configuration`.** Everything else in
there is what a *reader* needs in order to build a key, and nothing here builds
keys; that is the same boundary an extension dtype's and a codec's
configuration sit behind.

V3's `chunk_key_encoding` says how a chunk is *named*. Sharding says what a
chunk *is*. They are different questions and are reported separately: a sharded
array's grid is already described by its `chunks` and `shards` rows, and no
attempt is made to derive the names of the chunks inside a shard — see
[Sharding](#sharding).

### No default is invented

**A key the document does not carry contributes nothing.** V2 says an absent
`dimension_separator` means `.`; that is still not something this document
said, and printing it would be synthesising a normalised metadata document
rather than reporting the one on disk. So an array declaring only an `order`
shows only that:

```
└─ layout: order=F
```

and an array declaring neither key prints no row at all and carries no JSON
key. This is the rule `shards`, `dimension_names` and `codecs` already follow —
a field with nothing to report does not report, and no `layout: default` is
invented.

### Malformed metadata keeps the readable half

The two halves degrade independently, and whichever survives is shown. A V3
encoding whose `configuration` is missing, is not an object, or holds a
non-string `separator` still shows its name:

```
└─ layout: encoding=v2
```

and an encoding object with no readable `name` still shows a separator that
could be read:

```
└─ layout: separator="/"
```

There is no `?` here, unlike a codec chain: a codec's position has to be held
because dropping it would claim a shorter chain than the file declares, whereas
these are two independent named facts and leaving one out overstates nothing.
An array with *nothing* readable — no key, a key that is not an object, an
object nothing could be read from, a `.zarray` that is not JSON — gets no row
and no JSON key. None of it reclassifies the array, and none of it stops the
walk.

Both versions' keys are in documents already parsed for `shape` and `dtype`, so
the row costs no extra request anywhere.

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
├─ zarr: V3
└── volume [array]
    ├─ zarr:   V3
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
├─ zarr: V3
└── img [array]
    ├─ zarr:   V3
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
├─ zarr: V3
└── img [array]
    ├─ zarr:   V3
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
is never executed, and it appears in the `codecs` row as the one name the
document's `codecs` key holds — see
[A sharded array shows one codec](#a-sharded-array-shows-one-codec).

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
- **Children are sorted naturally**, so a run against the same store prints the
  same tree. A run of digits inside a name compares as the number it spells, so
  `2` comes before `10` and `level9` before `level10`; everything else compares
  by character, as it always did. Two names that spell the same number
  differently — `1` and `01` — are ordered by how many leading zeroes they
  carry, so the order is total and depends on nothing outside the names
  themselves: no locale, no filesystem, no operating system.
- **`--depth N` limits how far below the root the walk goes.** At the limit the
  directory is not read at all, which is what makes `--depth 0` cheap on a
  store with a million chunk files. A node that is shown keeps its own metadata
  rows, because those describe the node itself rather than anything below it.
  See [Depth](cli.md#depth).

## JSON representation

`--json` is a second renderer over one reading of the metadata, not a second
interpretation, so the tree and the document cannot disagree about what a store
contains.

Every node carries `name` and `children`, plus `kind` and — where it was
recognised as one — `zarr_format`; an array additionally carries an `array`
object. (Nodes also carry `ome`, `spatialdata`, `parquet` and
`anndata` sections where those apply — see
[OME-Zarr reference](ome-zarr.md#json-representation), and the whole-document
shape in the [Command-line reference](cli.md#json-output).)

| Field | Meaning |
| --- | --- |
| `name` | The directory name. On the root, the path as it was typed. |
| `kind` | `"group"`, `"array"` or `"unknown"` |
| `zarr_format` | `2` or `3`, the number Zarr itself stamps into its metadata files. Absent on an `[unknown]` node — see [Format version](#format-version). |
| `children` | The child nodes, in the order the tree lists them. Empty for an array, and empty at the depth limit. |
| `array` | `shape`, `chunks`, `dtype`, `shards` on a sharded V3 array, `fill_value` where the document declares one, `dimension_names` on a V3 array that declares them, `codecs` where a chain is declared, and `layout` where the document says anything about chunk order or naming. |
| `attributes` | The node's user attributes, with `--attributes` only. Values keep their JSON types; `null` where the document could not be read. Absent where there are none. |

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
  "name": "img",
  "zarr_format": 3
}
```

Two rules govern the `array` object:

- A field that was looked for and could not be read is `null`, the same thing
  the tree draws as `?`. `shape`, `chunks` and `dtype` are therefore always
  present.
- `shards`, `fill_value`, `dimension_names`, `codecs` and `layout` are
  **omitted entirely** where they do not apply, rather than written as `null`.
  The two say different things: `null` means "looked for, not readable", and an
  unsharded array has no shards to miss, just as an array naming no dimensions
  has no names to miss and an array declaring no processing has no chain to
  miss.

A `fill_value` of `null` is therefore not an omission but the value the
document itself wrote — see [Fill values](#fill-values):

```json
"fill_value": null
```

Inside `dimension_names` a `null` means a third thing again — a dimension the
file itself left unnamed, kept in its own position:

```json
"dimension_names": ["c", null, "y", "x"]
```

`codecs` uses the same convention for the same reason: the list is in
declaration order and a `null` is a codec declared at that position whose name
could not be read — see [Codecs](#codecs). For a V2 array the list is `filters`
in order followed by `compressor`:

```json
"codecs": ["delta", "blosc"]
```

`layout` is an object, never the row's text, and its keys are the ones the
document gave — the same omission rule one level down. The two versions never
share a spelling, so which keys are present also says which model was read:

```json
"layout": {"order": "C", "separator": "."}
```
```json
"layout": {"encoding": "default", "separator": "/"}
```

`separator` is spelled the same in both on purpose: the two versions call the
key different things, but a reader asking `.array.layout.separator` is asking
one question — which is the same reason the two share a row. No
`configuration` is carried through and no specification default is filled in;
see [Chunk layout](#chunk-layout).

Object keys come out in alphabetical order — `serde_json`'s default, kept
rather than fought.

## Validation checks

`--validate` was added in **v0.4.0**.

It prints findings instead of the tree, over the same metadata the tree reads.
Two of the seven rules are about plain Zarr; the rest are OME-Zarr and
SpatialData and are documented in
[OME-Zarr reference](ome-zarr.md#validation) and the
[Command-line reference](cli.md#the-seven-rules).

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
| `dimension_names` has one entry per `shape` dimension (V3 arrays that declare any) | `PASS` |
| `dimension_names` has a different number of entries | `ERROR` |

```
$ zarr-tree --validate sharded.zarr
PASS  /  Zarr root metadata is readable
PASS  /img  array shape and chunks agree on 2 dimensions
PASS  /img  array shape and shards agree on 2 dimensions

Validation: 3 passed, 0 warnings, 0 errors
```

Only the count of `dimension_names` is checked. A `null` entry is a dimension
the file deliberately left unnamed — still a dimension, so `["c", null, "x"]`
holds against a shape of `[3, 64, 64]` — and the names themselves are never
read: nothing checks them for uniqueness or meaning, and nothing compares them
with an OME-Zarr `axes` list, which is separate metadata a level may also
carry. An array that declares no `dimension_names` produces no finding, exactly
as an unsharded array says nothing about shards.

```
$ zarr-tree --validate misnamed.zarr
PASS  /  Zarr root metadata is readable
PASS  /img  array shape and chunks agree on 3 dimensions
ERROR /img  array shape has 3 dimensions but its dimension names cover 2 dimensions

Validation: 2 passed, 0 warnings, 1 error
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
least one `ERROR` was — see [Exit statuses](cli.md#exit-statuses).

## Deliberately not implemented

These are boundaries, not gaps. Several of them are what make the walk cheap
enough to run against a remote store at all.

- **Chunk reads of any kind**, and no chunk object is ever listed.
- **Decompression.** Nothing is ever decoded, so no chunk is decompressed and
  no codec is instantiated to find out whether it could be.
- **Data-value inspection.** No element of any array is read, so nothing is
  counted, summed or ranged.
- **Complete Zarr specification validation.** `--validate` checks a store
  against its own declarations, never a document against a schema.
- **Store repair, rewriting or writing of any kind.**
- **Codec execution, instantiation and configuration.** A chain is listed by
  name; no codec is run, no `configuration` is read or shown, and no name is
  checked against a registry — see [Codecs](#codecs).
- **Chunk-key construction.** The layout row reports what a document says
  about chunk order and chunk naming; no chunk key is built from it, guessed
  at, enumerated or looked for, and a V3 `configuration` is not read beyond its
  `separator` — see [Chunk layout](#chunk-layout). Nothing about the layout is
  checked either: an `order` outside `C`/`F`, an unusual separator and an
  unknown encoding name are all displayed as stored.
- **Fill-value interpretation.** The value is shown as the document wrote it
  and nothing more: `"NaN"` is a string, not a float; no value is decoded,
  normalised, or checked against the array's dtype — see
  [Fill values](#fill-values).
- **Interpretation of user attributes.** They are shown on request, and only as
  stored — see [User attributes](#user-attributes). Beyond the OME-Zarr and
  SpatialData markers this program already reads, no attribute key is given a
  meaning, promoted to a row of its own, checked, or used to filter or reorder
  anything. A V3 array's `dimension_names` are shown on the same terms: as
  stored, not matched against OME-Zarr axes, and checked only for how many
  there are.
- **dtype translation and interpretation.** V2 dtypes are passed through in
  NumPy notation and are never mapped onto V3 names. A V3 extension dtype is
  reported by its name alone: its `configuration` is not shown, not checked and
  not interpreted, and no dtype of either version is validated against a
  registry or a specification.
- **Consolidation forms beyond the two above**, and checking a consolidated
  document against the store it describes.

Storage backends are a separate boundary — local, S3 and HTTP(S) are the whole
list, with no ZIP store, GCS or Azure, and no scraping of HTML directory-index
pages. The full matrix is in [Project status](status.md#zarr).

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
