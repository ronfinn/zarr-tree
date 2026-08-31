# SpatialData reference

How `zarr-tree` recognises [SpatialData](https://spatialdata.scverse.org/)
stores and the elements inside them, and what it reports about the two payload
formats those elements use: Parquet, for points and shapes, and
[AnnData](https://anndata.readthedocs.io/) written into Zarr, for tables.

This is not a summary of SpatialData. It documents one program's reading of it.

- [Scope](#scope)
- [Store recognition](#store-recognition)
- [Element recognition](#element-recognition)
- [Images and labels](#images-and-labels)
- [Points](#points)
- [Shapes](#shapes)
- [Parquet metadata summaries](#parquet-metadata-summaries)
- [Tables](#tables)
- [AnnData metadata summaries](#anndata-metadata-summaries)
- [Region linkage](#region-linkage)
- [JSON representation](#json-representation)
- [Validation](#validation)
- [Real-store observations](#real-store-observations)
- [Metadata-only guarantees](#metadata-only-guarantees)
- [Graceful degradation](#graceful-degradation)
- [Current limitations](#current-limitations)
- [References](#references)

## Scope

SpatialData keeps a spatial omics experiment in one Zarr container: microscopy
images, segmentation masks, transcript locations, geometries and annotation
tables. It is a set of conventions layered over ordinary Zarr, and — for its
raster elements — over [OME-Zarr](ome-zarr.md). `zarr-tree` treats it that way:
the [Zarr reading](zarr.md) is unchanged, and a SpatialData node is a Zarr node
that happened to carry a key worth reporting.

Six things are recognised and no others — a store root, and image, labels,
points, shapes and table elements — each tagged in the node's own line:

```
experiment.zarr [group, SpatialData 0.2]
morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]
transcripts [group, SpatialData points]
table [group, SpatialData table]
```

Three rules govern all of it.

**Recognition is metadata-only.** Nothing is inferred from a directory name. In
a real store the `images`, `labels`, `points`, `shapes` and `tables` container
groups carry no attributes at all, so a name would be the only thing left to go
on — and an ordinary Zarr store whose children happen to be called `points` and
`shapes` is not a SpatialData store:

```
$ zarr-tree plain.zarr
plain.zarr [group]
├─ zarr: V3
├── images [group]
│   └─ zarr: V3
├── points [group]
│   └─ zarr: V3
├── shapes [group]
│   └─ zarr: V3
└── tables [group]
    └─ zarr: V3
```

**The scientific payload is never loaded to recognise anything.** An element is
identified from Zarr metadata alone. Its payload — Parquet coordinates and
geometries, an AnnData expression matrix — is *summarised* afterwards, from
footers and metadata files, and only for a node the Zarr metadata already
named.

**Values are shown as stored**, unchecked against the versions or forms that
exist. `--validate` is the one place anything is checked, and it checks a store
against its own declarations — never a document against a schema.

For where these readers sit in the program, see
[Architecture](architecture.md#spatialdata); that page also carries the diagram
of how the five element kinds reach their content, and it is not repeated here.

## Store recognition

A store root is recognised from one key, and its presence rather than its
value:

```
spatialdata_attrs.spatialdata_software_version
```

`spatialdata_attrs` **alone proves nothing**. Every element inside a store
carries one of its own, holding just the version of that element's encoding.
Only the root records the software that wrote it. Without that distinction
every image, label, points and shapes group in the store would be reported as a
store of its own.

The tag carries the *container* format version, read from
`spatialdata_attrs.version` and printed as stored:

```
$ zarr-tree --depth 1 experiment.zarr
experiment.zarr [group, SpatialData 0.2]
├─ zarr: V3
├── images [group]
│   └─ zarr: V3
├── labels [group]
│   └─ zarr: V3
├── points [group]
│   └─ zarr: V3
├── shapes [group]
│   └─ zarr: V3
└── tables [group]
    └─ zarr: V3
```

A root whose marker is present but whose version is missing, null, or not a
string is still recognised, and tags bare: `[group, SpatialData]`. The version
is a label, not a discriminator.

The container version also decides which Zarr layout the store uses, and so
where every marker below lives:

| Container | Zarr | Attributes |
| --- | --- | --- |
| 0.1 | V2 | `.zattrs`, keys at the top level |
| 0.2 | V3 | `attributes` inside `zarr.json` |

That difference costs the readers one argument and nothing else. The keys, and
the values matched against them, are identical in both layouts.

A store written before SpatialData recorded a software version carries no root
marker and is left untagged rather than guessed at. Its points, shapes and
table elements are still recognised, because those name themselves in a key
such a store does carry — see [Element recognition](#element-recognition).

## Element recognition

Elements are recognised independently of the root, and a group is only ever one
kind. Three markers are tried in order — root, then a self-naming element, then
a raster — and the first that matches is the answer.

| Element | Metadata basis | Where the payload lives |
| --- | --- | --- |
| Image | `spatialdata_attrs` **and** OME-Zarr `multiscales` | Zarr arrays under the element |
| Labels | the same, **plus** OME-Zarr `image-label` | Zarr arrays under the element |
| Points | `encoding-type` = `ngff:points` | Parquet, outside the Zarr hierarchy |
| Shapes | `encoding-type` = `ngff:shapes` | Parquet, outside the Zarr hierarchy |
| Table | `spatialdata-encoding-type` = `ngff:regions_table` | AnnData written into Zarr |

Two key names carry the self-naming kinds, for a historical reason rather than
a semantic one: a table's group is written by AnnData, which claims
`encoding-type` for its own `"anndata"`, so SpatialData records the kind one
key over. One list of values is tried under both names — they are drawn from a
single namespace and no store writes them crosswise.

Each value is **matched exactly**, never by prefix and never by the presence of
a key alone. AnnData writes `encoding-type` throughout the subtree beneath a
table — `"dataframe"`, `"csr_matrix"`, `"array"` — and none of those is an
element.

Element tags carry no version. The number an element records is the version of
its own encoding, a different quantity from the container version on the root
line: in a container 0.2 store the points element is 0.2 and the shapes element
is 0.3, and printing those next to `SpatialData 0.2` would suggest a
disagreement that is not there. Recognition does not depend on either version:
both self-naming markers have been written unchanged since the earliest
releases, in Zarr V2 and V3 alike. What changed between element versions is
where the *payload* lives, and no payload is read to classify anything.

Nothing beyond the kind is read from an element's own attributes: its axis
names (which the writer sorts, so they do not record dimension order), its
feature and instance keys and its coordinate transformations are left alone.
The one exception is a table, which declares what it annotates in the same
attributes object — see [Region linkage](#region-linkage).

## Images and labels

Both are raster elements, and both name themselves nowhere: SpatialData writes
them through the OME-Zarr writers, which have no `encoding-type` of their own.
They are recognised from two facts together.

*That* it is a SpatialData element comes from `spatialdata_attrs`, required to
be an object so that one holding a string or a number proves nothing. On its
own that is weak evidence. Paired with OME-Zarr image metadata — a
`multiscales` key holding a non-empty array, tested exactly as the OME-Zarr
reader tests it — it is what separates an element of a store from an ordinary
microscopy image that has nothing to do with SpatialData. Without it, every
OME-Zarr image ever written would be tagged as a SpatialData element.

*Which* raster it is comes from OME-Zarr. A segmentation is a multiscale image
like any other, distinguished only by an `image-label` object beside its
`multiscales`. That key is consulted **for its presence alone**, in this one
narrow recognition path, and nothing inside it — colours, properties, the
source image — is read or surfaced. This is not image-label relationship
support; see [Labels and image-label](ome-zarr.md#labels-and-image-label) for
what the OME-Zarr reader does and does not do with it.

The specification says a label image *should* carry `image-label` rather than
*must*, so a segmentation that omits it is reported as an image. The
alternative would be to guess from the `labels/` directory name, which this
program does not do — a group under `labels/` with no `image-label` is an
image, and a group under `images/` with one is labels.

Everything shown under a raster element — axis names, declared pyramid level
count, dataset paths — comes from the OME-Zarr reader, unchanged by
SpatialData's presence. Both vocabularies appear on the line, because both were
found:

```
$ zarr-tree --depth 2 experiment.zarr/images
experiment.zarr/images [group]
├─ zarr: V3
└── morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]
    ├─ zarr: V3
    ├─ axes: c, y, x
    ├─ pyramid levels: 1
    ├─ datasets: s0
    └── s0 [array]
        ├─ zarr:   V3
        ├─ shape:  [4, 2048, 2048]
        ├─ chunks: [1, 512, 512]
        └─ dtype:  uint16
```

## Points

A points element declares itself with `encoding-type` = `ngff:points`, matched
exactly. Transcript locations and molecule detections are written this way.

Its coordinates live **outside** the Zarr hierarchy. SpatialData writes a
points frame as a *partitioned* Parquet dataset, so the payload is a directory:

```
points.parquet/
    part.0.parquet
    part.1.parquet
    ...
```

That path is declared nowhere in the element's metadata. It is a convention of
SpatialData's writer, and is hard-coded for that reason.

**The convention is reached only after recognition, never before it.** A
payload is looked for because a group's own metadata said it is a points
element, not because a directory looked promising. An arbitrary directory
called `points.parquet` elsewhere in a store is an ordinary Zarr node and is
walked into as usual; a `.parquet` file elsewhere is not read at all.

Once the element is recognised, its payload directory **is not drawn as a child
node**. It is the element's data, not a node beneath it, and the rows above it
already say what is in it:

```
$ zarr-tree --depth 2 experiment.zarr/points
experiment.zarr/points [group]
├─ zarr: V3
└── transcripts [group, SpatialData points]
    ├─ zarr: V3
    ├─ rows: 3,714
    ├─ columns: 4
    ├─ parquet files: 2
    └─ schema: x:double, y:double, feature_name:string, cell_id:int32
```

That filtering applies to a group whose metadata named it a points element, and
to nothing else. It is also the same filtering `--validate` walks through, so
validation sees exactly the nodes printing the store would show.

## Shapes

A shapes element declares itself with `encoding-type` = `ngff:shapes`, matched
exactly. Cell boundaries, circles and landmark geometries are written this way.

Its geometries are a GeoDataFrame written in one go, so the payload is a
**single file** beside the element's metadata:

```
shapes.parquet
```

Like the points directory, that name is a writer convention rather than
anything the element declares, and it is reached only after recognition. A
single file at a known name needs no listing, which is the practical difference
between the two payload kinds — see
[Static HTTP and points payloads](#static-http-and-points-payloads).

```
$ zarr-tree --depth 2 experiment.zarr/shapes
experiment.zarr/shapes [group]
├─ zarr: V3
└── cell_boundaries [group, SpatialData shapes]
    ├─ zarr: V3
    ├─ rows: 1,200
    ├─ columns: 2
    ├─ parquet files: 1
    └─ schema: geometry:byte_array, cell_id:int32
```

A shapes file is also never drawn in the tree, but for a duller reason than the
points directory: the walk lists directories, and this is a file. The asymmetry
between the two payloads is on disk, not in this program.

**GeoParquet semantics are not interpreted.** A geometry column shows the type
the Parquet footer exposes — the logical type where the column declares one,
the physical type otherwise — so a WKB geometry reads `byte_array`. The
GeoParquet `geo` key/value metadata block is not read, and no geometry type,
CRS or bounding box is reported.

## Parquet metadata summaries

For a points or shapes element with a readable payload, four rows are printed:

| Row | Meaning |
| --- | --- |
| `rows` | rows across every file of the payload, summed from the footers |
| `columns` | how many top-level columns the schema declares |
| `parquet files` | how many files the payload is written across |
| `schema` | those columns, `name:type`, in declaration order |

`rows` is grouped with thousands separators, because a transcripts payload is
counted in millions and `3714642` is not a number anybody reads.

Column types are Parquet's own vocabulary, reported rather than translated: the
logical type where a column declares one (`string`, `uint8`, `timestamp`), and
the physical type otherwise (`double`, `byte_array`) — for the same reason a
Zarr `dtype` is printed as stored.

Columns are the top-level fields a reader of the table sees; a nested column
counts once and is not expanded into its leaves. Past twelve columns the tree's
`schema` row counts the rest rather than naming them, since a terminal is only
so wide. `--json` always carries the whole schema.

**No Parquet record is read.** No row group is opened, no page is decoded and
no coordinate or geometry is looked at. Row counts and the schema are what the
file *declares* in its footer: nothing is counted, nothing is cross-checked,
and a footer that disagrees with the pages below it is reported as it stands.
Row-group layout, encodings, compression, statistics and key/value metadata are
not read.

### Footer access

Parquet puts its metadata at the *end* of a file: a thrift-encoded block, then
that block's length, then the four bytes `PAR1`. So only the end is fetched.

The window is `PARQUET_TAIL`, currently **64 KiB**. One read of the last 64 KiB
finds both the eight-byte tail and, nearly always, the metadata block above it.
A schema too large for that window costs exactly one more read, of the length
the tail just named. Two reads at worst, of a few kilobytes each.

How that read is made depends on the backend, and is the whole reason
`Store::read_suffix` can only ask for the *end* of an object:

| Backend | The read |
| --- | --- |
| local | open, seek to `size - 64 KiB` (clamped to 0), read to the end |
| S3 / HTTP | one `HEAD` for the object size, then one bounded range `GET` |

A method that can only ask for a suffix cannot be talked into fetching a
gigabyte-and-a-half transcripts payload, whoever calls it.

**A file smaller than the window is returned in full**, because the range is
clamped to the start of the object — for a three-kilobyte landmark payload that
is the whole file, in one bounded request. That is a consequence of asking for
a suffix, not a decision to read the file's rows: what comes back is still
handed only to the footer decoder. The guarantee is the shape of the request,
not a byte count — see
[Remote Parquet payloads](remote-stores.md#remote-parquet-payloads) for the
transport detail.

### Partitioned points payloads

A points payload of several parts is read part by part:

- **File count** is the number of `.parquet` files found in the directory. A
  `_metadata` file, which dask writes only when asked, is not a part and is
  filtered out.
- **Rows** are summed across every part's footer.
- **Schema and column count come from the first part alone.** The parts of one
  payload are one table written in pieces and share a schema, so the later
  parts have nothing to add.

That last point is a limitation as well as an economy: **no cross-part schema
check is made.** If the parts disagreed about their columns, the first part's
schema is what would be reported, and nothing would say so.

Parts are read in the order the listing gave them, which is lexicographic — ten
parts order `part.0`, `part.1`, `part.10`, `part.2`. Nothing depends on that:
the rows are summed and the schema is one schema.

### Readable, unavailable and absent

Three payload states are distinguished, and the middle one is why they are
three rather than two: a points element on a server that cannot list a
directory looked exactly like one with no payload until they were told apart.

| State | Tree | `--json` |
| --- | --- | --- |
| Readable | the four rows above | `"parquet": { … }` |
| Unavailable | `parquet files: ?` | `"parquet": null` |
| Absent | no payload row at all | no `parquet` key at all |

```
$ zarr-tree --depth 1 https://static.example/data/xenium.zarr
https://static.example/data/xenium.zarr [group, SpatialData 0.2]
├─ zarr: V3
└── points [group]
    ├─ zarr: V3
    └── transcripts [group, SpatialData points]
        ├─ zarr: V3
        └─ parquet files: ?
```

**One unavailable marker, and no more.** The rows, the width and the schema are
not separately unknown — they are all unknown for the one reason, which is that
the payload was not read, so `rows: ?`, `columns: ?` and `schema: ?` alongside
it would be four rows saying one thing.

Only a **points** element can report `parquet files: ?`, and only for two
reasons: the parts could not be listed, or a part's footer could not be read. A
points directory the store reports as simply not there is absence, not
unavailability.

A **shapes** payload never reports `?`. Its filename came from the convention
rather than from a listing, and a failed suffix read cannot tell a missing file
from a non-Parquet one from an encrypted footer. With no way to distinguish
those, absence stays the honest reading, and the element keeps its tag and
loses only its rows.

### Static HTTP and points payloads

The two payload kinds differ in what the backend must support:

| Element | Payload path | Needs a listing? |
| --- | --- | --- |
| shapes | `shapes.parquet`, one file at a known name | No |
| points | `points.parquet/`, a directory of parts | Yes — the filenames are never guessed at |

On a plain static HTTP server that answers `GET` but no `PROPFIND`,
[consolidated metadata](zarr.md#consolidated-metadata) can carry the whole Zarr
hierarchy, so the store's SpatialData structure walks in full. It does **not**
help here: a consolidated document is an index of Zarr metadata documents, and
no such document has ever enumerated a Parquet part file. So the hierarchy
walks and the points summary does not — `parquet files: ?`, and a `WARN` under
`--validate`, never an error. A shapes payload on the same server still
summarises, because its name was never in question and a range `GET` is all it
needs. The transport details are in [Remote stores](remote-stores.md).

## Tables

A table element declares itself with `spatialdata-encoding-type` =
`ngff:regions_table`, matched exactly.

Two vocabularies meet in that one group, and they are read separately.
SpatialData contributes the marker and the [region linkage](#region-linkage);
AnnData contributes everything about the table's own shape, and is summarised
only where AnnData's metadata is actually present. A table written without it
keeps its tag and draws fewer rows.

Naming a table does not collapse it. The AnnData subtree below is an ordinary
Zarr hierarchy of groups and arrays, walked and printed like any other:

```
$ zarr-tree --depth 1 experiment.zarr/tables/table
experiment.zarr/tables/table [group, SpatialData table]
├─ zarr: V3
├─ observations: 1,200
├─ variables: 313
├─ X: dense [1200, 313] float32
├─ obs columns: 3
├─ var columns: 2
├─ annotates: cell_boundaries
├─ region key: region
├─ instance key: cell_id
├── X [array]
│   ├─ zarr:   V3
│   ├─ shape:  [1200, 313]
│   ├─ chunks: [1200, 313]
│   └─ dtype:  float32
├── obs [group]
│   └─ zarr: V3
└── var [group]
    └─ zarr: V3
```

The summary is licensed by the table marker and by nothing else. A group that
merely holds children called `X`, `obs` and `var`, or that is merely called
`table`, is an ordinary Zarr group and is read as one.

## AnnData metadata summaries

`zarr-tree` does **not** use the `anndata` Python package, `anndata-rs` or
`zarrs`. It reads AnnData's own Zarr metadata conventions directly, with
`serde_json` and the `Store` trait already in use — see
[Current dependency boundary](architecture.md#current-dependency-boundary).

AnnData records the shape of a table in metadata, so nothing has to be counted,
and nothing is:

| Row | Read from |
| --- | --- |
| `observations` | the length declared by the array `obs` names in `_index`, falling back to `X`'s first dimension |
| `variables` | the same for `var`, falling back to `X`'s second dimension |
| `X` | how the matrix is stored, the shape it declares, and — dense only — its dtype |
| `obs columns` | the length of the `column-order` `obs` declares |
| `var columns` | the same for `var` |

That is **five metadata reads below the table node** — `obs`, `var`, the index
array each of them names, and `X` — and **no listing at all**. Every path is
named by a metadata file, so a table costs the same handful of `GET`s on a
static HTTP server as on a local disk, and comes wholly out of a consolidated
snapshot when the store carries one.

Each row is independent: a table whose `var` cannot be read still reports the
observations its `obs` declared. Nothing is checked against anything either — a
table whose `X` declares a shape its `obs` index disagrees with is reported as
it stands, and [`--validate`](#validation) is where that is called out.

### obs and var

Both are AnnData dataframe groups, written with `encoding-type` = `dataframe`,
and two keys in their attributes describe them entirely:

- **`column-order`** — the columns, in order. The tree shows the count;
  `--json` carries the declared names in full.
- **`_index`** — the *name* of the array holding the index. That array's own
  Zarr metadata carries its length, and that length is the axis length. So
  `observations` is read out of a `shape` field rather than counted from
  anything.

The columns are the ones `column-order` declares, and **never the children of
`obs` on disk**. The two are usually the same list, but only one of them is
what the dataframe says about itself — and a listing would also sweep up the
index array and the `categories`/`codes` groups of every categorical column.

Two reads per dataframe: the group, and the one array it named. Neither array
is opened, and no dataframe value, category or index label is read. An entry in
`column-order` that is not a string becomes `?` rather than being dropped, so
the count stays the count the file declared.

### The X matrix

Three representations are understood, and the row shows what was found:

| `X` written as | Row |
| --- | --- |
| a Zarr array | `X: dense [1200, 313] float32` |
| a group with `encoding-type` = `csr_matrix` | `X: csr [167780, 313]` |
| a group with `encoding-type` = `csc_matrix` | `X: csc [167780, 313]` |

A dense `X` *is* an array, and being an array is what makes it dense — its own
Zarr metadata supplies both shape and dtype. A sparse `X` is a group of three
arrays whose attributes declare both the representation and the shape, so the
`data`, `indices` and `indptr` arrays inside it are **never opened**.

That is one of the core metadata-only guarantees, and it has two visible
consequences:

- **No sparse dtype.** A sparse matrix keeps its element type on the `data`
  array, and that array is not read.
- **No non-zero count.** `nnz` is not in the metadata, and finding it would
  mean reading `indptr`.

The three values are matched exactly. Any other representation — a COO matrix,
a backed or dask-written form, anything a later AnnData adds — draws **no `X`
row at all** rather than a guess, and the two counts still print. The
representation alone is worth a row, though, so a shape or a dtype that could
not be read shortens the `X` row rather than removing it.

### Count fallbacks

`observations` prefers the `obs` index length and falls back to `X`'s first
dimension; `variables` prefers the `var` index length and falls back to `X`'s
second. The fallback costs nothing — `X` has already been read — and whichever
answered first is the answer. The two are not compared.

That is the right behaviour for a *display*: a table whose index array is
unreadable still says how big it is, from the one other place the store
declared it.

It would be the wrong behaviour for a *check*, and `--validate` deliberately
does not use it. The AnnData dimensions rule re-reads the `obs` and `var` index
lengths itself, because comparing `X` against a number taken from `X` would
pass whatever the store held. On a table whose `obs` index is unreadable the
two paths part company:

```
├─ observations: 1,200                    # the tree, fallen back to X's first dimension

WARN  /tables/table  AnnData obs index length unavailable, so the observations were not checked
```

The display fell back and reported a number. The check refused to, and said
so.

## Region linkage

A table declares what it annotates in three keys, written by SpatialData beside
AnnData's own in the table group's attributes:

| Key | Row | Meaning |
| --- | --- | --- |
| `region` | `annotates` | the element or elements this table annotates |
| `region_key` | `region key` | the `obs` column naming each observation's region |
| `instance_key` | `instance key` | the `obs` column naming the instance within it |

`region_key` and `instance_key` are the **names** of two columns, and names are
all that is reported: the columns themselves are chunk data and are never read.
Nothing in `uns` or `spatialdata_attrs` is decoded to reconstruct any of this.

`region` is written as a bare string for a single element and as a list for
several. Both mean the same thing, so both are levelled into a list, and the
tree joins them — `annotates: cell_boundaries, nuclei`. Past twelve regions the
row counts the rest rather than naming them, the same cap the Parquet schema
row uses, and `--json` carries the list whole. An entry that is not a string
becomes `?` rather than being dropped, so a table over three regions is never
reported as one over two.

Each row is independent, and a table that annotates nothing draws none of them:
SpatialData writes those keys as nulls rather than leaving them out, and a null
is not something to print.

## JSON representation

`--json` is a second renderer over the same reading, so it cannot disagree with
the tree. Three sections apply here, and each is absent when it does not apply:

| Section | Present on | Fields |
| --- | --- | --- |
| `spatialdata` | every recognised node | `kind` (`root`, `image`, `labels`, `points`, `shapes`, `table`), `version` — only a root records one — and, on a table, `regions`, `region_key`, `instance_key` |
| `parquet` | points and shapes elements with a payload | `rows`, `columns`, `files`, and the whole `schema` as `name`/`type` objects; `null` when the payload was unavailable |
| `anndata` | table elements | `encoding_version`, `observations`, `variables`, `obs_columns`, `var_columns`, `x` |

```
$ zarr-tree --json --depth 0 experiment.zarr/tables/table | jq '.anndata'
{
  "encoding_version": "0.1.0",
  "obs_columns": ["region", "cell_id", "transcript_counts"],
  "observations": 1200,
  "var_columns": ["gene_ids", "feature_types"],
  "variables": 313,
  "x": { "dtype": "float32", "kind": "dense", "shape": [1200, 313] }
}
```

`spatialdata` and `anndata` stay separate objects on a table, because they are
two vocabularies read from two sets of keys: what SpatialData said about the
elements the table annotates, and what AnnData said about the table itself.

Inside `anndata`, `null` means a field was looked for and could not be read, so
every key is always present. `x.dtype` is the exception — *not applicable* to a
sparse matrix rather than unread, so it appears only on a dense `X`. The column
lists are whole however long, where the tree shows only their counts.

## Validation

`--validate` was added in **v0.4.0**.

Three of the seven rules concern SpatialData, and only those three are
documented here. The Zarr and OME-Zarr rules are in
[zarr.md](zarr.md#validation-checks) and
[ome-zarr.md](ome-zarr.md#validation); the CLI behaviour, exit status and
output forms are in the [Command-line reference](cli.md#structural-validation).

**Table regions.** Every name in a table's `region` must resolve to a
recognised image, labels, points or shapes element somewhere in the store,
matched on the element's own name — `cell_boundaries`, not
`shapes/cell_boundaries`. A table is not a region and is not in that set. No
Parquet column and no `obs` value is read to discover a name.

```
PASS  /tables/table  table region "cell_boundaries" names an existing SpatialData element
ERROR /tables/table  table region "tissue" does not name an existing SpatialData element
WARN  /tables/table  table names a region this tool could not read
```

The `WARN` is the unreadable entry that `annotates` shows as `?`.

**AnnData dimensions.** `X.shape[0]` must match the length the `obs` index
declares, and `X.shape[1]` the `var` index. The index lengths are re-read here
rather than taken from the displayed summary, for the reason under
[Count fallbacks](#count-fallbacks).

```
PASS  /tables/table  AnnData X rows match the 1200 observations the obs index declares
ERROR /tables/table  AnnData X has 1200 rows but the obs index declares 1199 observations
WARN  /tables/table  AnnData obs index length unavailable, so the observations were not checked
WARN  /tables/table  AnnData X could not be read, so the table's dimensions were not checked
```

**Parquet availability.** A payload that is there and readable passes; one that
is there and could not be inspected warns; one that is genuinely absent is not
a finding at all, because an element is free to have none and this cannot tell
a store that never wrote one from a store that lost it.

```
PASS  /points/transcripts  SpatialData points Parquet payload is readable (2 files)
WARN  /points/transcripts  SpatialData points payload metadata unavailable
```

`WARN` means "could not be checked", never "broken". A points payload on a
listing-less server warns; that is a fact about the server, not the store. Only
an `ERROR` sets exit status 2.

## Real-store observations

The SpatialData reading was cross-checked against real public stores rather
than fixtures alone. These are observations recorded during that work, not a
compatibility certification, and nothing was downloaded to write this page.

| Store | Table | `X` | Notable |
| --- | --- | --- | --- |
| MERFISH | 2,389 observations, 268 variables | dense `[2389, 268]` | a 3,714,642-row points payload; `shapes/cells` and `shapes/anatomical` |
| Xenium | 167,780 observations, 313 variables | `csr` | `shapes/cell_boundaries` and `shapes/cell_circles` |
| MIBI-TOF | 3,309 observations, 36 variables | — | a table annotating multiple regions |
| Visium | 6,484 observations, 31,053 variables | — | the widest `var` of the set |

Between them they exercised both `X` representations, single- and multi-region
linkage, multiple shapes elements under one store, and a points payload large
enough that reading it whole would have been unaffordable — which is the case
the footer-only read exists for.

## Metadata-only guarantees

| Data | Read? |
| --- | --- |
| Zarr node metadata (`zarr.json`, `.zgroup`, `.zarray`, `.zattrs`) | Yes |
| AnnData `obs` / `var` group attributes and index array metadata | Yes |
| AnnData `X` array or sparse-group metadata | Yes |
| Parquet file footer | Yes |
| Zarr image or labels chunks | No |
| AnnData `X` values | No |
| Sparse `data`, `indices`, `indptr` values | No |
| `obs` / `var` column values, categories, index labels | No |
| Parquet records, pages, row groups | No |
| Anything under an array | No |

Arrays are leaves in a SpatialData store as much as anywhere else: an array's
chunk objects are never listed, however many millions of them there are.
Nothing above is counted — every number this guide describes is a field in a
metadata file or a Parquet footer. The one thing read that is not a Zarr
metadata document is that footer, reached through two methods that exist for
nothing else: `Store::files`, which names the parts of a points payload, and
`Store::read_suffix`, which can only ask for the *end* of an object.

## Graceful degradation

A store that is malformed in one place costs that one thing and no more.

| What is wrong | What happens |
| --- | --- |
| Element attributes missing or unreadable | The node is not classified as SpatialData. It is still walked and printed as an ordinary Zarr node. |
| Root marker present, version missing or not a string | Recognised, tagged bare: `[group, SpatialData]`. |
| Points payload cannot be listed, or a part's footer cannot be read | `parquet files: ?`, `"parquet": null`, and a `WARN` under `--validate`. The element keeps its tag. |
| Shapes payload missing, not Parquet, or encrypted | Reported as absent: no payload row, no `parquet` key. The element keeps its tag. |
| `obs` or `var` index unreadable | The count falls back to `X`'s declared shape for display; `--validate` warns rather than checking. |
| `column-order` unreadable | No `obs columns` / `var columns` row. The index length is unaffected. |
| `X` in an unknown representation | No `X` row at all, rather than a guess. The two counts still print. |
| Table annotates nothing | No `annotates`, `region key` or `instance key` row — the keys are written as nulls. |
| A region name that is not a string | Rendered `?` and counted, so the region count stays honest; `--validate` warns. |

None of these ends the walk. The wider model — `?` for a field, `[unknown]` for
a node — is in
[Unknown and malformed nodes](zarr.md#unknown-and-malformed-nodes).

## Current limitations

All of these remain true today. Several are
[under consideration](roadmap.md#under-consideration) rather than closed, and
the full matrix is in [Project status](status.md#spatialdata).

- **No data value of any kind is read**: no expression value, annotation value,
  category, index label, coordinate or geometry.
- **Sparse `X` reports no dtype and no `nnz`.** Both live on arrays that are
  never opened. Only dense, `csr_matrix` and `csc_matrix` `X` are described at
  all; any other representation draws no `X` row.
- **Categorical columns are not decoded**, and `layers`, `obsm`, `obsp`,
  `varm`, `varp`, `uns` and `raw` are not interpreted or counted. All are
  walked as the groups they are, which is what keeps the summary to five
  metadata reads and no listing.
- **H5AD / HDF5 is not read.** Only AnnData written into Zarr.
- **Nothing is checked outside `--validate`.** Ordinary output reports what the
  metadata declares, including a table whose `X` and index lengths disagree.
- **GeoParquet semantics are not interpreted.** Column types come from the
  Parquet footer alone; the `geo` metadata block, geometry types, CRS and
  bounding boxes are not read.
- **A partitioned points payload takes its schema from the first part**, and no
  cross-part schema check is made. Row-group layout, encodings, compression and
  statistics are not reported, and no record, page or row group is decoded.
- **A points payload cannot be enumerated on a listing-less server**, and its
  part filenames are never guessed at. An arbitrary `.parquet` file elsewhere
  in a store is never read and never becomes a tree node.
- **Element axes, feature keys, geometry types and coordinate transformations
  are not shown**, for any element kind.
- **No element is joined to any other.** A table names the regions it
  annotates, and outside `--validate` nothing checks that those elements exist
  or links them to the table.
- **A store root written before SpatialData recorded a software version is not
  recognised as a root.** Its points, shapes and table elements still are,
  because those name themselves in a key such a store does carry; its images
  and labels do not, since they are recognised in part by a `spatialdata_attrs`
  those older stores do not write.
- **A segmentation that omits `image-label` is reported as an image**, and
  nothing inside `image-label` is read.
- **Writing, repairing or editing a store is out of scope entirely.**

## References

- [SpatialData](https://spatialdata.scverse.org/) — the project and its
  documentation.
- [SpatialData Zarr format](https://spatialdata.scverse.org/en/stable/design_doc.html)
  — the design document behind the on-disk conventions.
- [AnnData](https://anndata.readthedocs.io/) — including
  [on-disk format](https://anndata.readthedocs.io/en/stable/fileformat-prose.html),
  which is what the table summary reads.
- [Apache Parquet](https://parquet.apache.org/docs/file-format/) — the file
  format, and the footer this program reads.

## See also

- [Zarr reference](zarr.md) — the layer underneath: node classification,
  arrays, consolidated metadata, and the degradation model.
- [OME-Zarr reference](ome-zarr.md) — what a raster element's axes, pyramid
  levels and dataset paths come from.
- [Remote stores](remote-stores.md) — S3, HTTP and WebDAV, and what a payload
  summary costs on each.
- [Architecture](architecture.md#spatialdata) — where these readers sit, the
  `Store` trait, and the Parquet and AnnData access paths.
- [Getting started](getting-started.md) — building it, the option set, exit
  status, and `--validate` in general.
- [Project status](status.md#spatialdata) — the capability matrix.
- [Roadmap](roadmap.md) — direction, with nothing promised.
