# OME-Zarr reference

How `zarr-tree` recognises [OME-Zarr](https://ngff.openmicroscopy.org/) groups
and what it shows about them: which keys it looks for, where it looks for them,
what it prints, and how much of the specification it deliberately leaves alone.

This is not a summary of OME-NGFF. It documents one program's reading of it.

- [Scope](#scope)
- [Recognition](#recognition)
- [Versions and metadata locations](#versions-and-metadata-locations)
- [Axes](#axes)
- [Multiscale datasets](#multiscale-datasets)
- [Pyramid level semantics](#pyramid-level-semantics)
- [Plates and wells](#plates-and-wells)
- [Labels and image-label](#labels-and-image-label)
- [JSON representation](#json-representation)
- [Validation](#validation)
- [Current limitations](#current-limitations)
- [Specification references](#specification-references)

## Scope

OME-Zarr is metadata layered over ordinary Zarr groups. `zarr-tree` treats it
exactly that way: the [Zarr reading](zarr.md) is unchanged, and an OME-Zarr
group is a group that happened to carry a key worth reporting. Every array
under an image is still classified, printed and stopped at as any Zarr array
is.

Three things follow from that.

**Recognition is metadata-only.** Nothing is inferred from a directory name. A
group called `labels`, `plate` or `A1` is an ordinary `[group]` unless its own
attributes say otherwise. This is a rule of the project, not an accident of the
implementation — see [CLAUDE.md](../CLAUDE.md) and
[Design principles](architecture.md#design-principles).

**Values are shown as stored.** Versions, axis names and dataset paths are
printed back exactly as they were written, unchecked against the versions or
forms that exist. An unfamiliar value still shows.

**The tree is recognition, not validation.** Ordinary output claims only that
the metadata says something, never that the store matches it. `--validate` adds
a handful of structural checks and nothing more — see
[Validation](#validation).

Tags the program emits, and no others:

```
[group, OME-Zarr 0.4]
[group, OME-Zarr 0.5]
[group, OME-Zarr 0.5 plate]
[group, OME-Zarr 0.5 well]
[group, OME-Zarr]
```

The last is a group whose OME-Zarr metadata is present but records no readable
version. On a SpatialData store an element carries both vocabularies, and both
appear: `[group, OME-Zarr 0.5-dev-spatialdata, SpatialData labels]` — see the
[SpatialData reference](spatialdata.md).

## Recognition

One attributes object is examined, and the first key found decides which of
three kinds the group is. They are tried in this order:

| Key | Requirement | Kind |
| --- | --- | --- |
| `multiscales` | a non-empty array | image |
| `plate` | an object | plate |
| `well` | an object | well |

None of them: an ordinary Zarr group, with no OME-Zarr tag and no extra rows.

The requirements are not decoration. A `multiscales` that is missing, is the
wrong JSON type, or is an empty array all mean the group is not an image; a
`plate` holding a string proves nothing. The three kinds do not overlap in
practice — a group is an image, a plate or a well, and the metadata says which
by the key it wrote.

Only the **first** `multiscales` entry is read. A store declaring several
multiscales gets the first one's axes and datasets, and the others are not
looked at.

## Versions and metadata locations

Where the OME-Zarr keys live follows the Zarr version underneath, and so does
where the version itself is recorded.

| OME-Zarr | Zarr | Attributes | Image version field |
| --- | --- | --- | --- |
| 0.1 – 0.4 | V2 | `.zattrs`, keys at the top level | first `multiscales` entry's `version`, often absent in real 0.4 stores |
| 0.5 | V3 | `attributes.ome` inside `zarr.json` | `attributes.ome.version` |

Both layouts are read, and both are read the same way once the right object is
in hand: `.zattrs` *is* the attributes object for V2, and `attributes.ome` is
it for V3. The version is passed alongside because it is the only other thing
the two layouts disagree about.

A plate and a well record their version in a third place again. V3 records it
once for the whole `ome` namespace; V2 has no namespace to record it in and
puts it inside the `plate` or `well` object instead. Both are read, and **the
object's own `version` wins where there is one**, with the namespace answering
otherwise:

```
$ zarr-tree v2plate.zarr
v2plate.zarr [group, OME-Zarr 0.4 plate]
├─ zarr: V2
├─ rows: 1
├─ columns: 1
├─ wells: 1
└── A [group]
    ├─ zarr: V2
    └── 1 [group, OME-Zarr well]
        └─ zarr: V2
```

That plate declared `plate.version` of `0.4`; the well beside it declared no
version anywhere, so its tag is bare.

**The version is printed exactly as stored** and is never checked against the
versions that exist. `0.5-dev-spatialdata`, which SpatialData writes, shows as
`0.5-dev-spatialdata`. A version that is absent, or is not a string, leaves the
tag as `[group, OME-Zarr]` — with the kind still appended for a plate or a
well.

OME-Zarr 0.1 and 0.2 are recognised as images if they carry `multiscales`, but
they predate `axes` entirely, so such a group shows dataset rows and no axes
row.

## Axes

Axis names come from the first `multiscales` entry's `axes`, whose form changed
over the course of the specification. Both forms are read in one pass:

| OME-Zarr | `axes` |
| --- | --- |
| 0.1, 0.2 | no `axes` field |
| 0.3 | a list of names — `["c", "y", "x"]` |
| 0.4, 0.5 | a list of objects — `{"name": "y", "type": "space", "unit": "micrometer"}` |

**Only `name` is displayed.** An axis's `type` (`space`, `time`, `channel`) and
`unit` are read for nothing and shown nowhere, in either form.

```
├─ axes: c, y, x
```

Three rules govern the row:

- An entry whose name cannot be read **keeps its position** and shows as `?`,
  so the number of axes displayed always matches the number the file declares.
  Dropping it would report a three-dimensional image as two-dimensional.
- `axes` that is absent, empty, or not a list prints **no row at all**. Nothing
  is inferred from an array's dimensionality — an image whose metadata declares
  no axes is not given axes because its arrays have three dimensions.
- Names are not sorted, renamed, checked for the ordering OME-NGFF prescribes,
  or matched against the arrays below.

## Multiscale datasets

The same `multiscales` entry's `datasets` lists the resolution levels the image
is stored at. Unlike `axes`, this has had the same shape since OME-NGFF 0.1 — a
list of objects each carrying a `path` — so one reading serves every version.

Two rows come out of it, and they are one fact shown twice:

```
├─ pyramid levels: 3
├─ datasets: 0, 1, 2
```

The count is the length of the list of paths, so the two cannot drift apart.
Both rows appear together or not at all: `datasets` that is absent, empty or
not a list prints neither.

**The count comes from the metadata, never from counting child directories.**
The two commonly disagree, and the disagreement is the point:

- an image group usually holds a `labels` group beside its levels;
- a 0.5 store adds an `OME` directory;
- a path may be nested, such as `a/b`;
- a truncated or partial copy may declare more levels than it holds.

The directories are already listed below these rows. What the metadata claims
is the part you cannot otherwise see.

**Paths are printed exactly as stored.** `0`, `1`, `2` is only a convention —
`s0`, `full`, `half` and nested paths are all legal — so nothing is sorted,
renumbered or interpreted:

```
$ zarr-tree named.zarr
named.zarr [group, OME-Zarr 0.3]
├─ zarr: V2
├─ axes: y, x
├─ pyramid levels: 2
├─ datasets: full, half
├── full [array]
...
```

An entry whose path cannot be read shows as `?`, following the same rule the
axes follow, so the count still matches what the file declares:

```
$ zarr-tree partial.zarr
partial.zarr [group, OME-Zarr]
├─ zarr: V3
├─ axes: c, y, x
├─ pyramid levels: 2
└─ datasets: 0, ?
```

That store declares two levels and has neither of them on disk, so nothing
follows the rows and the last one closes the branch with `└─`.

What OME-NGFF 0.4 added to each dataset entry —
`coordinateTransformations` — is **not read**.

## Pyramid level semantics

The two modes make different claims, and it is worth being precise about which:

| Mode | Claim |
| --- | --- |
| Tree (default) | The metadata *declares* these levels at these paths. |
| `--validate` | Each declared path was resolved against the store, and here is what was found. |

Ordinary tree output **does not check that a declared path exists**. An image
declaring three levels and holding none prints three levels. That is not a bug
to be worked around; it is the difference between reporting metadata and
checking it, and `--validate` is where the checking lives.

Nothing about physical resolution is calculated, in either mode. No scale
factor, pixel size, downsampling ratio or physical extent is derived, because
`coordinateTransformations` — the only place that information lives — is not
read at all. A three-level pyramid is three levels; whether each is half the
previous one is not a question this program asks.

## Plates and wells

High-content screening stores a plate of wells rather than a single image. Both
groups name themselves the way an image does — with a key in their attributes,
`plate` or `well` — and the kind is appended to the tag after the version:

```
$ zarr-tree plate.zarr
plate.zarr [group, OME-Zarr 0.5 plate]
├─ zarr: V3
├─ rows: 2
├─ columns: 3
├─ wells: 6
└── A [group]
    ├─ zarr: V3
    └── 1 [group, OME-Zarr 0.5 well]
        ├─ zarr: V3
        └── 0 [group, OME-Zarr 0.5]
            ├─ zarr: V3
            ├─ axes: c, y, x
            ├─ pyramid levels: 1
            └─ datasets: 0
```

A plate's three rows are the lengths of the lists its metadata declares:
`plate.rows`, `plate.columns` and `plate.wells`. Like the pyramid level count
they come from the metadata and **never** from counting directories — a plate
that declares 96 wells says 96 whether or not 96 were written. Each count is
independent, so a plate declaring only some of the three lists shows only those
rows.

`plate.wells` is a list of `{"path": …}` objects, exactly the shape a
multiscale's `datasets` has, and it is read by the same code. The paths
themselves are kept rather than just counted, because `--validate` looks for
the group each one names; the tree shows the count and nothing more.

**A well adds no rows of its own.** What a well holds is its images, and the
tree is already printing them as child groups. Nothing is invented to fill the
space.

**The kind is decided by the key, never by a name.** A plate's rows and columns
really are called `A`, `B`, `1`, `2`, and any store is free to use those names
for anything at all — so the row group `A` above is an ordinary `[group]`,
because that is all its own metadata says it is. There is no naming convention
being matched here, in either direction.

Children sort naturally, as everywhere else in the tree, so a plate with more
than nine columns lists `9` before `10` rather than between `1` and `2`. That
is presentation only: the order the wells are drawn in is not read from, nor
checked against, the plate's declared `rows` and `columns`.

Nothing inside `plate` or `well` beyond the three counts and the well paths is
read. Acquisitions, field-of-view indices, the well's own `images` list, row
and column names: all left alone.

## Labels and image-label

This section is precise about a distinction that is easy to overstate.

OME-NGFF describes a segmentation as an ordinary multiscale image
distinguished by an `image-label` object beside its `multiscales`, holding the
colours and properties of the label values.

**`zarr-tree` does not surface image-label relationships.** What it does is
narrower, and in one place only:

| Situation | Behaviour |
| --- | --- |
| A plain OME-Zarr group with `multiscales` and `image-label` | Tagged `[group, OME-Zarr 0.4]` — an image, like any other. `image-label` is not read. |
| A group with `spatialdata_attrs`, `multiscales` **and** `image-label` | Recognised as a SpatialData **labels** element. `image-label` is tested for presence alone. |
| A `labels` group holding a `labels` list of child names | Not read. It is an ordinary `[group]`. |

So:

```
$ zarr-tree segmentation.zarr
segmentation.zarr [group]
├─ zarr: V2
└── labels [group]
    ├─ zarr: V2
    └── nuclei [group, OME-Zarr 0.4]
        ├─ zarr: V2
        ├─ axes: y, x
        ├─ pyramid levels: 1
        ├─ datasets: 0
        └── 0 [array]
            ├─ zarr:   V2
            ├─ shape:  [64, 64]
            ├─ chunks: [32, 32]
            └─ dtype:  <u4
```

`nuclei` carries a full `image-label` block with colours and properties. It is
reported as an OME-Zarr image, because that is what it is, and the `labels`
group above it is an ordinary group.

Where `image-label` *is* consulted, it is consulted for one bit of information
— present or absent — to tell a SpatialData labels element from a SpatialData
image element, which are otherwise written identically. No label value, colour,
property or source image is ever read. And because OME-NGFF says a label image
*should* carry `image-label` rather than *must*, a segmentation that omits it
is reported as an image; the alternative would be to guess from the `labels/`
directory name, which this program does not do.

Nothing here is a claim of image-label support. Richer `image-label` metadata
is [under consideration](roadmap.md#under-consideration).

## JSON representation

An OME-Zarr group carries an `ome` section in `--json`, beside the `name`,
`kind` and `children` every node has.

| Field | Present | Meaning |
| --- | --- | --- |
| `tag` | always | The label text: `"OME-Zarr 0.5 plate"` |
| `kind` | always | `"image"`, `"plate"` or `"well"` |
| `version` | always | As stored, or `null` |
| `axes` | always | The names, or `null` |
| `pyramid_levels` | always | The declared count, or `null` |
| `datasets` | always | The declared paths, or `null` |
| `rows` | plates | The declared row count |
| `columns` | plates | The declared column count |
| `wells` | plates | The declared well **count**, not the paths |

```
$ zarr-tree --json image.zarr | jq '.ome'
{
  "axes": ["c", "y", "x"],
  "datasets": ["0", "1", "2"],
  "kind": "image",
  "pyramid_levels": 3,
  "tag": "OME-Zarr 0.4",
  "version": "0.4"
}
```

`null` means the field was looked for and could not be read — the same thing
the tree draws as `?`, and the same rule the Zarr `array` section follows. A
plate's three counts are omitted rather than nulled when the corresponding list
is missing, because a plate that declared no `columns` has no column count to
be unreadable.

`axes` and `datasets` carry the `?` placeholders too, so a JSON reader sees the
same declared count the tree shows. See
[JSON representation](zarr.md#json-representation) for the node fields common
to every kind.

## Validation

`--validate` was added in **v0.4.0**.

These are **metadata-only structural checks**: what an OME-Zarr group's own
metadata declares, resolved against the nodes the walk found. This is not
OME-NGFF conformance validation, and no document is checked against a schema.
For conformance use the
[OME-NGFF validator](https://ome.github.io/ome-ngff-validator/).

Three of the seven rules concern OME-Zarr. The others are Zarr
([Zarr reference](zarr.md#validation-checks)) and SpatialData
([SpatialData reference](spatialdata.md#validation)).

**Dataset paths exist.** Each `multiscales[0].datasets[].path` is resolved
against the node map the walk built. Nothing is opened, and no chunk is
touched.

| Finding | Severity |
| --- | --- |
| The path resolves to an array | `PASS` — `OME dataset path "0" exists` |
| The path resolves to a group or an unknown node | `ERROR` — `is not an array` |
| The path resolves to nothing | `ERROR` — `does not exist` |
| The entry had no readable path (`?`) | `WARN` |
| The multiscale declares no readable `datasets` | `WARN` |

A pyramid level is an array, so a path landing on a group is an error of the
same kind as one landing on nothing.

**Pyramid dimensions agree.** Every level that resolved to an array with a
readable shape is compared for dimensionality. The multiscale's `axes` are the
reference where there are any — OME-NGFF gives one axis per dimension — and the
first level is the reference otherwise, which still catches a pyramid whose
levels disagree without claiming which of them is right.

| Finding | Severity |
| --- | --- |
| Every level agrees | `PASS` — `pyramid levels agree with the multiscale's axes on 3 dimensions` |
| A level disagrees | `ERROR`, one finding per level |

No resolution, scale factor or downsampling ratio is looked at. That is image
science; this is structure.

**Plate wells exist.** Each path in `plate.wells` is resolved the same way.

| Finding | Severity |
| --- | --- |
| The path resolves to a group | `PASS` — `plate well path "A/1" exists` |
| The path resolves to an array or unknown node | `ERROR` — `is not a group` |
| The path resolves to nothing | `ERROR` — `does not exist` |
| The entry had no readable path (`?`) | `WARN` |
| The plate declares no readable `wells` | `WARN` |

**A well is checked for nothing.** What a well holds is fields of view, and
which of them a well must have is an HCS rule this first validation mode
deliberately does not know. A well's own `images` list is not read.

Two runs, over the fixtures above:

```
$ zarr-tree --validate image.zarr
PASS  /  Zarr root metadata is readable
PASS  /  OME dataset path "0" exists
PASS  /  OME dataset path "1" exists
PASS  /  OME dataset path "2" exists
PASS  /  pyramid levels agree with the multiscale's axes on 3 dimensions
PASS  /0  array shape and chunks agree on 3 dimensions
PASS  /1  array shape and chunks agree on 3 dimensions
PASS  /2  array shape and chunks agree on 3 dimensions

Validation: 8 passed, 0 warnings, 0 errors
```

```
$ zarr-tree --validate plate.zarr
PASS  /  Zarr root metadata is readable
PASS  /  plate well path "A/1" exists
ERROR /  plate well path "A/2" does not exist
ERROR /  plate well path "A/3" does not exist
ERROR /  plate well path "B/1" does not exist
ERROR /  plate well path "B/2" does not exist
ERROR /  plate well path "B/3" does not exist
PASS  /A/1/0  OME dataset path "0" exists
PASS  /A/1/0  pyramid levels agree with the multiscale's axes on 3 dimensions
PASS  /A/1/0/0  array shape and chunks agree on 3 dimensions

Validation: 5 passed, 0 warnings, 5 errors
```

That plate declares six wells and only one was written — which is what a
truncated download looks like, and what `--validate` exists to say out loud.

`WARN` always means "could not be checked", never "broken". Exit status is 0
when nothing worse than a warning was found and 2 when at least one `ERROR`
was — see [Structural validation](cli.md#structural-validation).

## Current limitations

All of these remain true today. Several are
[under consideration](roadmap.md#under-consideration) rather than closed, and
the full matrix is in [Project status](status.md#ome-zarr).

- **`coordinateTransformations` are not read**, in a multiscale or in a
  dataset entry.
- **No physical scale is calculated**: no pixel size, no downsampling factor,
  no physical extent, no unit conversion.
- **Axis `type` and `unit` are not shown.** Only the name is displayed, from
  either axis form.
- **Axis names and ordering are not checked** against the order OME-NGFF
  prescribes, or against the arrays below.
- **Only the first `multiscales` entry is read.** Additional entries are not
  reported.
- **`omero` and channel metadata are not read**: no channel names, windows,
  colours or rendering settings.
- **`image-label` is read for its presence alone**, and only where SpatialData
  raster discrimination needs it. No image-label relationship is surfaced — see
  [Labels and image-label](#labels-and-image-label).
- **A `labels` group's `labels` list is not read.** It is an ordinary group.
- **HCS acquisitions and field-of-view indices are not interpreted**, and a
  well's `images` list is not read.
- **No declared count is checked against what is present** in ordinary tree
  output — a plate declaring 96 wells says 96. `--validate` resolves the well
  paths; nothing else does.
- **Bioformats2raw layout metadata is not recognised.**
- **No OME-NGFF conformance validation.** `--validate` checks a store against
  its own declarations, never a document against a schema.

## Specification references

- [OME-NGFF specification](https://ngff.openmicroscopy.org/) — the
  authoritative document, all versions.
- [OME-NGFF validator](https://ome.github.io/ome-ngff-validator/) — for
  conformance checking, which `zarr-tree` does not do.

## See also

- [Zarr reference](zarr.md) — the layer underneath: node classification,
  arrays, sharding, consolidated metadata.
- [SpatialData reference](spatialdata.md) — the layer above: how a raster
  element is told from an ordinary OME-Zarr image.
- [Getting started](getting-started.md) — building it, the option set, and exit
  status.
- [Architecture](architecture.md#ome-zarr) — where the OME-Zarr reader sits and
  why it is one function over two layouts.
- [Project status](status.md#ome-zarr) — the capability matrix.
- [Roadmap](roadmap.md) — direction, with nothing promised.
