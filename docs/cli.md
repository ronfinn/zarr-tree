# Command-line reference

The complete behaviour of the `zarr-tree` command: its one argument, its three
options, both output formats, the structural validation mode, and the exit
statuses and stream conventions a script or CI job depends on.

[Getting started](getting-started.md) is the tutorial — build the binary and
inspect a first store. This page is the reference, and assumes you already have
a working binary.

Every example below was run against a small local fixture store. Nothing here
is transcribed from an older version of the output.

## Synopsis

```sh
zarr-tree [OPTIONS] <STORE>
```

Exactly one store, plus any of the options, in any order. Options may appear
before or after the store, and repeating a flag asks for the same thing twice
rather than being an error.

## Options

| Option | Argument | Meaning |
| --- | --- | --- |
| `--depth` | `<N>` | Descend at most `N` levels below the root. Omitted, the whole store is walked. |
| `--json` | — | Print the same walk as one JSON document instead of a tree. |
| `--validate` | — | Check the structure the metadata declares, instead of printing the tree. |
| `--attributes` | — | Show each node's user attributes as stored, alongside the rows already printed. |
| `-h`, `--help` | — | Print the help text and exit 0. |
| `-V`, `--version` | — | Print the version and exit 0. |

There are no other options, no short aliases beyond `-h` and `-V`, and no
`--option=value` form: `--depth` takes its number as the next argument.

An argument beginning with `-` that is not one of the above is read as a
mistyped option rather than as a path. The cost is that a directory whose name
begins with `-` cannot be inspected — the same trade `-h` and `-V` already
make.

## Store argument

`STORE` names where the metadata is read from. Four spellings are accepted, and
the leading scheme is the only thing that decides between them:

| Form | Read from |
| --- | --- |
| `example.zarr`, `/data/example.zarr` | A directory on this machine |
| `s3://bucket/path/example.zarr` | An S3 key prefix |
| `http://server.example/example.zarr` | An HTTP server |
| `https://server.example/example.zarr` | An HTTPS server |

Anything that does not begin with `s3://`, `http://` or `https://` is a local
path — including a relative path that happens to contain `s3://` somewhere
after its start. There is no scheme allow-list and no "unsupported scheme"
error: an unrecognised scheme is simply a filename, and fails as a missing path
would.

```
$ zarr-tree gs://bucket/store.zarr
error: path does not exist: gs://bucket/store.zarr
```

The three backends take the same walk, print the same tree and accept the same
options. What differs is what a backend can be *asked*: an S3 prefix and a
WebDAV server can list their children, a plain static HTTP server cannot. That,
with AWS credentials, regions, custom endpoints and consolidated metadata, is
the subject of [Remote stores](remote-stores.md), and is not repeated here.

## Tree output

With no options, `zarr-tree` prints one line per node and a short indented row
per metadata field.

```
$ zarr-tree example.zarr
example.zarr [group]
├─ zarr: V3
├── image [group, OME-Zarr 0.5]
│   ├─ zarr: V3
│   ├─ axes: y, x
│   ├─ pyramid levels: 2
│   ├─ datasets: 0, 1
│   ├── 0 [array]
│   │   ├─ zarr:   V3
│   │   ├─ shape:  [1024, 1024]
│   │   ├─ chunks: [256, 256]
│   │   └─ dtype:  uint16
│   └── 1 [array]
│       ├─ zarr:   V3
│       ├─ shape:  [512, 512]
│       ├─ chunks: [256, 256]
│       └─ dtype:  uint16
└── labels [group]
    ├─ zarr: V3
    └── cells [array]
        ├─ zarr:   V3
        ├─ shape:  [1024, 1024]
        ├─ chunks: [256, 256]
        └─ dtype:  uint8
```

The first line is the store as it was typed, with a trailing slash trimmed.
Every line below it is a node named by its directory or S3 prefix.

**Node lines** carry a four-character connector — `├── ` or `└── ` — a name,
and a bracketed tag:

| Tag | Meaning |
| --- | --- |
| `[group]` | A Zarr group |
| `[array]` | A Zarr array. Always a leaf. |
| `[unknown]` | A directory with no Zarr metadata this tool can read |

A group that also matches a recognised convention collects extra tags inside
the same brackets, in the order they were found — `[group, OME-Zarr 0.5]`,
`[group, SpatialData points]`, `[group, OME-Zarr 0.4, SpatialData image]`. The
version in such a tag is the version **as stored**, not one this tool checked or
normalised.

**Metadata rows** carry a shorter three-character connector — `├─ ` or `└─ ` —
which is what tells them apart from node lines at a glance. Which rows appear
depends on what the node is:

| Rows | On |
| --- | --- |
| `zarr:` | Every recognised group and array, first — `V2` or `V3`, the metadata format the node was read as. See [Format version](zarr.md#format-version). |
| `shape:`, `chunks:`, `dtype:` | Every array |
| `shards:` | A Zarr V3 sharded array, between `chunks:` and `dtype:` |
| `fill:` | An array whose metadata declares a `fill_value`, after `dtype:`. Drawn as compact JSON, so `0`, `"NaN"` and `null` are each visibly the type they are, and never interpreted or checked against the dtype. See [Fill values](zarr.md#fill-values). |
| `dimensions:` | A Zarr V3 array declaring `dimension_names`, after `fill:`. An unnamed dimension shows as `?` in its own position. |
| `axes:`, `pyramid levels:`, `datasets:` | An OME-Zarr image |
| `rows:`, `columns:`, `wells:` | An OME-Zarr HCS plate |
| `rows:`, `columns:`, `parquet files:`, `schema:` | A SpatialData points or shapes element with a payload |
| `observations:`, `variables:`, `X:`, `obs columns:`, `var columns:` | A SpatialData table |
| `annotates:`, `region key:`, `instance key:` | A SpatialData table |

A field that was looked for and could not be read prints as `?` rather than
stopping the walk:

```
$ zarr-tree partial.zarr
partial.zarr [group]
├─ zarr: V2
└── plain [array]
    ├─ zarr:   V2
    ├─ shape:  [1024, 1024]
    ├─ chunks: ?
    └─ dtype:  ?
```

What each row means, and which metadata key it came from, belongs to the format
guides: [Zarr](zarr.md), [OME-Zarr](ome-zarr.md) and
[SpatialData](spatialdata.md). The tree is written as the walk proceeds, so a
large store starts printing immediately — see
[Pipes and BrokenPipe](#pipes-and-brokenpipe).

## Depth

`--depth N` limits how far below the root the walk descends. The root is depth
0, its direct children are depth 1, and so on.

```
$ zarr-tree --depth 0 example.zarr
example.zarr [group]
└─ zarr: V3
```

```
$ zarr-tree --depth 1 example.zarr
example.zarr [group]
├─ zarr: V3
├── image [group, OME-Zarr 0.5]
│   ├─ zarr: V3
│   ├─ axes: y, x
│   ├─ pyramid levels: 2
│   └─ datasets: 0, 1
└── labels [group]
    └─ zarr: V3
```

`--depth 2` on this store reaches every node, so it prints the same tree the
first example does.

Three properties are worth stating exactly:

- **A node at the limit keeps its own metadata rows.** Those describe the node
  itself, not anything below it. The image group at `--depth 1` above still
  shows its axes, pyramid level count and dataset paths, even though the arrays
  those paths name are one level too far to be listed. The last connector moves
  accordingly: with no children below them, the rows close the branch with
  `└─`.
- **At the limit the store is not asked for children at all.** No directory is
  read, no S3 listing is made, no `PROPFIND` is sent. That is what makes
  `--depth 0` cheap on a store with a million chunk files, and what makes it
  the one option that works against a static HTTP server with no listing and no
  consolidated metadata.
- **Arrays are leaves at any depth.** The walk already stops at an array, so
  the limit has nothing to say about one. Chunk objects are never listed
  whatever `N` is.

`--depth` combines with `--json`. It is refused with `--validate` — see
[Option combinations](#option-combinations).

## Attributes

`--attributes` shows each node's user attributes as the store holds them. It is
off by default, and the default output is exactly what it was without it.

```
$ zarr-tree --attributes experiment.zarr
experiment.zarr [group]
├─ zarr: V2
├─ attributes: {"batch":3,"experiment":"A","instrument":{"model":"X"}}
└── image [group, OME-Zarr 0.4]
    ├─ zarr: V2
    ├─ axes: y, x
    ├─ pyramid levels: 1
    ├─ datasets: 0
    ├─ attributes: {"multiscales":[{"axes":[…],"datasets":[…],"version":"0.4"}]}
    └── 0 [array]
        ├─ zarr:       V2
        ├─ shape:      [64, 64]
        ├─ chunks:     [32, 32]
        ├─ dtype:      |u1
        └─ attributes: {"unit":"nm"}
```

One flag covers both Zarr versions: V2's `.zattrs` file and V3's `attributes`
member arrive under the same `attributes:` row, and the same `attributes` key
in `--json`. Groups and arrays both have them.

**Nothing is interpreted.** The object is printed as compact JSON on one line,
with keys sorted so a store prints the same thing on every run. Values keep
their types in `--json` — numbers stay numbers, `null` stays `null`, nested
objects and lists stay themselves — and no key is turned into a row of its own.
`{"batch": 3}` is shown as it is written, never as a `batch: 3` field, because
that would make an arbitrary user key look like something this tool understands.

**Raw and interpreted are both shown.** A row like `axes:` is *this tool's
reading* of the group's `multiscales` key; the `attributes:` row is the document
that reading came out of. So `multiscales`, `ome`, `spatialdata_attrs` and
`encoding-type` all appear in the raw object, directly beneath the rows derived
from them. Nothing is filtered out — the flag means "show what the store
actually contains" — and no semantic recognition changes when it is passed.

| The node's attributes | Tree | `--json` |
| --- | --- | --- |
| Absent — no `.zattrs`, no `attributes` member | no row | no key |
| An empty object, `{}` | no row | no key |
| A non-empty object | `attributes: {…}` | the object, values intact |
| Unreadable — a `.zattrs` that is not JSON, or an `attributes` member that is not an object | `attributes: ?` | `null` |

An empty object earns no row because `{}` is what a great many nodes carry —
zarr-python writes it into every group and array it creates — and a row on all
of them would bury the few nodes that say something. An *unreadable* one is
kept distinct from both: `?` and `null` say there is a document here that could
not be read, which is not the same as there being nothing to show. This is the
same rule the rest of the tool follows for malformed metadata, and a bad
`.zattrs` never stops the walk or downgrades an otherwise recognisable node.

`--attributes` combines with `--depth` and `--json`. It is refused with
`--validate` — see [Option combinations](#option-combinations).

### Cost

Attributes come out of metadata the walk already reads, with one exception: a
**Zarr V2 array** keeps its attributes in a `.zattrs` beside the `.zarray`,
which a default walk never opens. Asking for attributes opens it, so
`--attributes` costs one extra read per V2 array — and only per V2 array, only
when the flag is passed. V2 groups and every V3 node cost nothing extra, because
their attributes are in a file already parsed.

## JSON output

`--json` prints the same walk as one JSON document.

```
$ zarr-tree --json example.zarr/labels
{
  "children": [
    {
      "array": {
        "chunks": [
          256,
          256
        ],
        "dtype": "uint8",
        "shape": [
          1024,
          1024
        ]
      },
      "children": [],
      "kind": "array",
      "name": "cells",
      "zarr_format": 3
    }
  ],
  "kind": "group",
  "name": "example.zarr/labels",
  "zarr_format": 3
}
```

It combines with `--depth`, and honours the limit the same way the tree does.
This is the same walk and the same reading of the metadata — a second renderer,
not a second interpretation — so the two outputs cannot disagree about what a
store contains.

Every node carries three fields — four where it was recognised as a Zarr node —
and then one section per kind of metadata that applies to it:

| Field | Present on | Holds |
| --- | --- | --- |
| `name` | every node | The directory name. On the root, the path as it was typed. |
| `kind` | every node | `group`, `array` or `unknown` |
| `zarr_format` | recognised groups and arrays | `2` or `3`, the Zarr metadata version the node was read as. Absent on `unknown` |
| `children` | every node | The child nodes, in the order the tree lists them |
| `array` | arrays | `shape`, `chunks`, `dtype`, `shards` when sharded, `fill_value` when declared, and `dimension_names` when declared |
| `ome` | OME-Zarr groups | `tag`, `kind`, `version`, `axes`, `pyramid_levels`, `datasets`, and `rows`/`columns`/`wells` on a plate |
| `spatialdata` | SpatialData nodes | `kind`, `version`, and `regions`/`region_key`/`instance_key` on a table |
| `parquet` | points and shapes elements with a payload | `rows`, `columns`, `files`, `schema` |
| `anndata` | SpatialData tables | `encoding_version`, `observations`, `variables`, `obs_columns`, `var_columns`, `x` |
| `attributes` | groups and arrays, with `--attributes` only | The node's user attributes as stored, values intact. `null` when unreadable; absent when there are none |

The per-format field meanings live with the formats: [Zarr JSON](zarr.md#json-representation),
[OME-Zarr JSON](ome-zarr.md#json-representation) and
[SpatialData JSON](spatialdata.md#json-representation).

Object keys come out in alphabetical order — `serde_json`'s default, kept
rather than overridden — which is why `children` leads. `shape`, `chunks` and
`shards` are real JSON arrays rather than the `[1024, 1024]` text the tree
draws, with their entries copied across exactly as stored: a malformed
`"shape": [1, "x"]` comes out as `[1, "x"]` rather than being dropped.

### Absence, null, and empty

The document distinguishes three different kinds of "nothing", and a script
that treats them alike will misread a store.

**A section that does not apply is omitted.** A group has no `array` key, a
plain Zarr group no `ome` key, an unsharded array no `shards` key inside
`array`. The absence says the question does not arise.

**A field that applies but could not be read is `null`.** This is the same rule
the tree follows when it prints `?`:

```
$ zarr-tree --json partial.zarr | jq '.children[0].array'
{
  "chunks": null,
  "dtype": null,
  "shape": [
    1024,
    1024
  ]
}
```

Every array has a shape, chunks and a dtype to be missing, so all three keys
are always present and `null` means unreadable. `shards`, `fill_value` and
`dimension_names` are the exceptions and are omitted rather than `null`,
because an unsharded array has no shards to miss. A `fill_value` that *is*
`null` is therefore the value the document wrote, not a missing one — see
[Fill values](zarr.md#fill-values).

**A payload that exists but could not be inspected is `"parquet": null`.** A
points element whose `points.parquet/` directory could not be listed, or whose
part footer could not be read, carries the key with a `null` value — so it does
not read as an element with no payload at all. An element that genuinely has no
payload carries no `parquet` key:

```
$ zarr-tree --json spatial.zarr | jq -r '.. | objects | select(.spatialdata) |
    "\(.spatialdata.kind)\t\(.name)\tpayload=\(has("parquet"))"'
root	spatial.zarr	payload=false
points	transcripts	payload=true
shapes	cells	payload=false
```

`transcripts` has the key with a `null` value; `cells` has no payload at all.
The three states — readable, unavailable, absent — are described in full under
[Readable, unavailable and absent](spatialdata.md#readable-unavailable-and-absent).

**`children` is always present, and is `[]` rather than absent** for an array,
for a node at the depth limit, and for a group that really has no children — so
a reader can walk every node the same way. The consequence: the document does
not distinguish "no children" from "not looked at because of `--depth`". If
that distinction matters, do not pass `--depth`.

### jq recipes

A handful of expressions that were run against the fixtures on this page.

Top-level child names:

```
$ zarr-tree --json example.zarr | jq -r '.children[].name'
image
labels
```

Every array in the store, at any depth:

```
$ zarr-tree --json example.zarr | jq -r '.. | objects | select(.kind == "array") | .name'
0
1
cells
```

Each array with its declared shape:

```
$ zarr-tree --json example.zarr |
    jq -c '[.. | objects | select(.kind == "array") | {name, shape: .array.shape}]'
[{"name":"0","shape":[1024,1024]},{"name":"1","shape":[512,512]},{"name":"cells","shape":[1024,1024]}]
```

Note that `..` descends into every metadata section as well as into
`children`, so `select(.kind == "array")` is safe only because no section has a
`kind` of `"array"` — `ome.kind` is `image`/`plate`/`well`, and
`spatialdata.kind` is `root`/`image`/`labels`/`points`/`shapes`/`table`. Check
any filter of your own against a store carrying all three sections.

Validation findings have their own recipes — see
[Validation with jq](#validation-with-jq).

## Structural validation

`--validate` checks what a store's metadata **declares** against what the store
**has**, and prints findings instead of a tree.

```
$ zarr-tree --validate example.zarr
PASS  /  Zarr root metadata is readable
PASS  /image  OME dataset path "0" exists
PASS  /image  OME dataset path "1" exists
PASS  /image  pyramid levels agree with the multiscale's axes on 2 dimensions
PASS  /image/0  array shape and chunks agree on 2 dimensions
PASS  /image/1  array shape and chunks agree on 2 dimensions
PASS  /labels/cells  array shape and chunks agree on 2 dimensions

Validation: 7 passed, 0 warnings, 0 errors
```

A store that declares something it does not have reports it, and the process
exits 2:

```
$ zarr-tree --validate broken.zarr
PASS  /  Zarr root metadata is readable
WARN  /exports  no Zarr metadata this tool can read
PASS  /image  OME dataset path "0" exists
ERROR /image  OME dataset path "1" does not exist
PASS  /image  pyramid levels agree with the multiscale's axes on 2 dimensions
PASS  /image/0  array shape and chunks agree on 2 dimensions

Validation: 4 passed, 1 warning, 1 error
$ echo $?
2
```

### What this is, and what it is not

The difference from the default output is one word:

- **Inspection** — the default — reports what the metadata *says*. Nothing is
  checked; unfamiliar values are printed as stored.
- **Validation** — `--validate` — checks whether selected structural
  *declarations* agree with the store itself.

It is a metadata-only structural check and nothing more. It is **not** Zarr,
OME-NGFF, SpatialData or AnnData specification conformance checking; not
scientific validation; not pixel, chunk or value validation — no chunk is
opened; and not Parquet record validation — no record, page or row group is
decoded. For OME-NGFF conformance use the
[OME-NGFF validator](https://ome.github.io/ome-ngff-validator/).

No document is checked against a schema. Every question it asks is of the form
"this store's own metadata says X exists; does it?", answered from the same
metadata files the tree already reads plus four an AnnData table names.

### Findings

Each finding is one line:

```
SEVERITY  PATH  MESSAGE
```

The severity is padded to five characters so paths line up, and the path and
the message are separated by two spaces. Paths are store-relative and always
start with `/`; the store root is `/` on its own. The message carries its own
subject — `OME dataset path "1" does not exist`, not `does not exist` — so a
line stays legible when it has been grepped out of a thousand others.

Beneath the findings comes a blank line and one summary, whose three counts are
pluralised individually, so a clean run reads `0 warnings, 0 errors`:

```
Validation: 4 passed, 1 warning, 1 error
```

### Severities

| Severity | Means | Affects exit status |
| --- | --- | --- |
| `PASS` | The checked relationship holds. | No |
| `WARN` | The check could not be made. Nothing is claimed either way. | No |
| `ERROR` | The metadata declares something the store does not have. | Yes — exit 2 |

`WARN` is the load-bearing one. A check this tool could not make is not a check
that failed, and a store on a server that will not list a directory must not be
reported as a broken store. One representative example of each, all three drawn
from the `broken.zarr` run above:

```
PASS  /image  OME dataset path "0" exists
WARN  /exports  no Zarr metadata this tool can read
ERROR /image  OME dataset path "1" does not exist
```

That `WARN` is a stray directory somebody left in the store — a `.git`, an
export, a scratch folder — and a warning is as much as this can honestly make
of one. The **root** saying nothing is different: a store root with no readable
Zarr metadata is an `ERROR`, because then it is not a store.

### The seven rules

All seven are over metadata the tree already reads. There is no rule registry,
no policy engine, and no way to add an eighth from outside the source.

| # | Area | Check | Severities it can produce |
| --- | --- | --- | --- |
| 1 | Zarr | Every node walked into could be identified, and an array's `shape`, `chunks` — when sharded, `shards` — and, when declared, `dimension_names` agree on how many dimensions there are. Codecs, fill values and dtypes are not checked, and neither is what a dimension name says. | `PASS`, `WARN`, `ERROR` |
| 2 | OME-Zarr | Every `multiscales[0].datasets[].path` names a node that exists and is an array. | `PASS`, `WARN`, `ERROR` |
| 3 | OME-Zarr | Every resolution level has the same number of dimensions, and the same number as the multiscale declares axes. No scale, resolution or downsampling factor is looked at. | `PASS`, `ERROR` |
| 4 | HCS | Every path in a plate's `wells` list names a group that exists. Acquisitions and fields of view are not checked; a well itself is checked for nothing. | `PASS`, `WARN`, `ERROR` |
| 5 | SpatialData | Every element named in a table's `region` exists as a recognised image, labels, points or shapes element. No name is inferred from a payload. | `PASS`, `WARN`, `ERROR` |
| 6 | AnnData | `X.shape[0]` matches the length the `obs` index declares, and `X.shape[1]` the `var` index. The lengths come from the index arrays' own metadata; nothing is counted. | `PASS`, `WARN`, `ERROR` |
| 7 | Parquet | A points or shapes payload that is there and readable passes; one that is there and could not be inspected warns. A payload that is genuinely absent produces no finding at all. | `PASS`, `WARN` |

Where a rule turns on a format detail, that detail is documented with the
format: [Zarr validation checks](zarr.md#validation-checks),
[OME-Zarr validation](ome-zarr.md#validation) and
[SpatialData validation](spatialdata.md#validation).

Two shapes of `WARN` recur across the rules. A declaration this tool could not
read at all — a dataset entry with no readable path, a plate with no readable
`wells` — warns rather than errors, because the declaration is what would have
been checked. And a value needed for a comparison but not obtainable — an array
with no readable chunk shape, an `obs` index length that would not read — warns
and skips the comparison, saying so in the message.

### How a run is ordered

Validation walks the store whole and in two passes:

1. The hierarchy is classified — the same walk the tree makes, down to which
   children a node has.
2. The cross-node questions are then asked against that classification. A
   table's `region` may name an element anywhere in the store, including one
   that comes after the table, so no single streaming pass could answer it.

The result is deterministic: findings come out in store-path order, and the
same store produces the same report every time. The implementation is described
in [Architecture § Validation](architecture.md#validation).

This whole-store walk is why `--validate` and `--depth` are refused together: a
walk that stopped early would report every node below the limit as missing.

### Validation JSON

`--validate --json` prints one document — the same findings in the same order,
plus the counts the summary line carries.

```
$ zarr-tree --validate --json spatial.zarr
{
  "findings": [
    {
      "message": "Zarr root metadata is readable",
      "path": "/",
      "severity": "pass"
    },
    {
      "message": "SpatialData points payload metadata unavailable",
      "path": "/points/transcripts",
      "severity": "warn"
    }
  ],
  "summary": {
    "errors": 0,
    "passed": 1,
    "warnings": 1
  }
}
```

The document has exactly two top-level keys, `findings` and `summary`; a
finding has exactly three, `message`, `path` and `severity`. The severity
strings are lower case — `"pass"`, `"warn"`, `"error"` — and the summary holds
`errors`, `passed` and `warnings`. `summary.errors` is what the exit status is
built from, so the two can never disagree.

This is not the tree document with findings attached. `--validate` replaces the
tree, so there is no node hierarchy here; a job that wants both runs the command
twice.

### Validation with jq

Only the errors, one per line:

```
$ zarr-tree --validate --json broken.zarr |
    jq -r '.findings[] | select(.severity == "error") | "\(.path)  \(.message)"'
/image  OME dataset path "1" does not exist
```

The counts alone, or just the error count:

```
$ zarr-tree --validate --json broken.zarr | jq -c '.summary'
{"errors":1,"passed":4,"warnings":1}

$ zarr-tree --validate --json broken.zarr | jq '.summary.errors'
1
```

Prefer the exit status to a `jq` count where you can — it needs no `jq` on the
machine and cannot be confused by a run that failed before it produced a
document. Use `jq` when you want the findings themselves.

## Exit statuses

| Status | Meaning |
| --- | --- |
| `0` | The store was walked. With `--validate`, nothing worse than a `WARN`. Also `--help` and `--version`. |
| `1` | The store could not be read, or the command line made no sense. |
| `2` | `--validate` ran to completion and reported at least one `ERROR`. |

There are no other statuses, and without `--validate` only 0 and 1 occur —
inspection has no verdict to report. Two consequences worth being explicit
about:

- **A `WARN` does not produce exit 2.** Only an `ERROR` does. A build that
  failed over "I could not list this directory" would fail on exactly the
  stores that most need looking at.
- **1 and 2 are not interchangeable.** `2` means the validator ran and the
  store has a problem; `1` means the validator could not run — a missing store,
  a bad credential, a mistyped option. A gate that treats any non-zero status
  as "invalid data" will report a typo as a corrupt store.

A run that ends because the reader closed the pipe exits `0` — see below.

## Streams

| Output | Goes to |
| --- | --- |
| The tree, the JSON document, the help text, the version | stdout |
| Validation findings and the summary line | stdout |
| Error messages, and the usage line that follows some of them | stderr |

Nothing is interleaved: a run either writes its output to stdout or its error
to stderr. So `zarr-tree --validate store.zarr > findings.txt` captures every
finding and leaves the terminal clean, and `2>/dev/null` never suppresses part
of a result.

## Pipes and BrokenPipe

The tree is written as the walk proceeds, not assembled first, so a large store
begins printing immediately and `zarr-tree` sits comfortably at the producing
end of a pipeline:

```sh
zarr-tree big.zarr | head
zarr-tree big.zarr | less
zarr-tree big.zarr | grep SpatialData
```

When the reader stops reading — `head` has the lines it wanted, `less` was
quit — the next write fails with `BrokenPipe`. That is not a failure of ours:
the reader said it had seen enough. `zarr-tree` stops there, **quietly**: no
message on stderr, exit status 0, and no partial-write panic.

```
$ zarr-tree example.zarr | head -2
example.zarr [group]
├─ zarr: V3
```

That run produces zero bytes on stderr and the producer's status is 0. Without
this handling the same pipeline would end in a panic message and status 101,
which would make `zarr-tree | head` unusable inside `set -o pipefail`. Every
other write failure keeps its own behaviour: a line on stderr and exit status 1.

`--json` has no equivalent — the whole document is built in memory and written
at the end.

## Automation

`--validate` distinguishes three outcomes, so capture the status and branch on
it rather than testing truthiness. The same shape works in a script, a CI step,
or a workflow task:

```sh
zarr-tree --validate "$store"
status=$?

case "$status" in
    0) echo "$store: no structural errors" ;;
    2) echo "$store: declares structure it does not have" >&2; exit 1 ;;
    *) echo "$store: could not be inspected (status $status)" >&2; exit 1 ;;
esac
```

The distinction to preserve is between the two failing statuses. **Exit 2** is
a finding about the data, and is the status an ingestion or QC gate should fail
on. **Exit 1** is a finding about the run: the validator did not complete — a
missing path, an expired credential, a server that would not answer — and
treating that as a data problem sends someone looking in the wrong place.

Because `--validate` reads metadata only, the check costs a handful of small
reads per node and no chunk traffic at all, which is what makes it reasonable
to run against a remote store on every ingestion.

Two shell notes. Under `set -e`, a non-zero status aborts the script before you
can read it — run the command as the condition of an `if`, which `set -e`
exempts, or use `set +e` around the call. Under `set -o pipefail`, remember
that a `BrokenPipe` exit is 0, so `zarr-tree big.zarr | head` stays clean.

To fail only on errors while still recording the warnings:

```sh
zarr-tree --validate --json "$store" > findings.json
status=$?
[ "$status" -eq 1 ] && exit 1
jq -r '.findings[] | select(.severity != "pass") | "\(.severity)\t\(.path)\t\(.message)"' \
    findings.json
exit "$status"
```

A minimal Nextflow process, for a store staged as a local path:

```groovy
process VALIDATE_ZARR {
    input:
    path store

    script:
    """
    zarr-tree --validate "${store}"
    """
}
```

Nextflow fails a task on any non-zero exit, so this fails on both 1 and 2;
branch inside the script block if you want them treated differently.

Two things this is not. It is not a certification of any kind — `zarr-tree`
checks a store against its own declarations, and passing says nothing about
specification conformance, scientific validity, or fitness for a regulated
process. And the snippets above are examples of calling a CLI, not integrations
this project ships or supports.

## Errors

Command-line errors print a message and the usage line, and exit 1:

```
$ zarr-tree
error: expected a store
usage: zarr-tree [OPTIONS] <STORE>

$ zarr-tree --depth two example.zarr
error: --depth needs a whole number, not "two"
usage: zarr-tree [OPTIONS] <STORE>
```

| Situation | Message |
| --- | --- |
| No store given | `expected a store` |
| More than one store given | `expected exactly one store` |
| An unrecognised option | `unknown option: --colour` |
| `--depth` with nothing after it | `--depth needs a number, as in --depth 2` |
| `--depth` given a non-number or a negative number | `--depth needs a whole number, not "two"` |
| `--depth` with `--validate` | `--depth cannot be combined with --validate` |
| `--attributes` with `--validate` | `--attributes cannot be combined with --validate` |

Store errors print a message and exit 1, with **no** usage line — the command
was well formed, the store was not:

```
$ zarr-tree missing.zarr
error: path does not exist: missing.zarr

$ zarr-tree https://
error: invalid url "https://": empty host
```

| Situation | Message shape |
| --- | --- |
| A local path that is not there | `path does not exist: <path>` |
| An `s3://` URI with no bucket | `expected s3://bucket/prefix, not "s3://"` |
| An `http(s)://` URL with no host | `invalid url "...": empty host` |
| A bucket or prefix that is not there | `no such bucket or prefix: <uri>` |
| An HTTP server that answers `GET` but cannot list | `cannot list <url>: the server answers GET but not the WebDAV listing needed to find child nodes` |

That last is the common remote surprise, and is a distinct message on purpose:
a static server that served the root's metadata perfectly well has not told us
the store is missing. Such a store can still be read with `--depth 0`, and in
full if it carries consolidated metadata — see
[Remote stores § Troubleshooting](remote-stores.md#troubleshooting).

Errors *inside* a walk do not stop it: a node whose metadata could not be read
is labelled `[unknown]` and descended into, a field that could not be read
prints `?`, and the walk continues. See
[Unknown and malformed nodes](zarr.md#unknown-and-malformed-nodes).

## Help and version

```
$ zarr-tree --help
zarr-tree
Explore the structure and metadata of a Zarr store.

USAGE:
    zarr-tree [OPTIONS] <STORE>
...
```

```
$ zarr-tree --version
zarr-tree 0.4.0
```

Both are answered on sight, wherever they appear on the command line, and
neither touches the store: `zarr-tree --help some.zarr` prints the help.

The version comes from the package manifest at compile time, so it is the
version of the most recently prepared release — not a description of what the
binary can do. A build from `master` between releases reports the last
release's version while carrying work that is not in it. To find out whether a
binary has a given option, ask it:

```sh
zarr-tree --help | grep -q -- --validate && echo present
```

## Option combinations

| Combination | Supported |
| --- | --- |
| `--depth` + `--json` | Yes |
| `--validate` + `--json` | Yes |
| `--validate` + `--depth` | **No** — refused with a message |
| `--attributes` + `--depth` | Yes |
| `--attributes` + `--json` | Yes |
| `--validate` + `--attributes` | **No** — refused with a message |
| The same flag repeated | Yes — it asks for the same thing twice |
| `--help` or `--version` with anything else | Yes — they answer and exit |
| Options before or after the store | Yes |

Both refusals involve `--validate`, for two different reasons:

```
$ zarr-tree --validate --depth 1 example.zarr
error: --depth cannot be combined with --validate
usage: zarr-tree [OPTIONS] <STORE>

$ zarr-tree --validate --attributes example.zarr
error: --attributes cannot be combined with --validate
usage: zarr-tree [OPTIONS] <STORE>
```

`--depth` is refused because the two options cannot both mean what they say at
once — see [How a run is ordered](#how-a-run-is-ordered). `--attributes` is
refused because `--validate` does not print nodes at all: it prints findings
about them, and an attributes row has nowhere to go in one. Neither is quietly
ignored.

## What a run will never do

These hold for every option and every backend, and are what make the cost of a
walk predictable:

- **Arrays are leaves** — an array's chunk objects are never listed, at any
  depth, locally or remotely — and **no chunk or pixel value is ever read**, by
  any mode.
- **Only Parquet footers are read**, never a whole file; no record, page or row
  group is decoded.
- **AnnData summaries come from Zarr metadata alone.** No expression value,
  annotation value, category or index label is read, and nothing is counted.
- **The store is opened read-only.** Nothing is created, modified or deleted.

[Architecture](architecture.md) explains how the code is arranged so these are
structural rather than remembered;
[Remote stores § Remote efficiency](remote-stores.md#remote-efficiency) gives
the per-node request counts.

## Current limitations

CLI-level only. Format limitations belong to the format guides, and the full
list is in [Project status](status.md).

- Three options and no configuration file: no `~/.zarr-treerc`, no environment
  variable of zarr-tree's own, no profile, and no shell completions.
- No filtering — no include or exclude glob, no "arrays only" — and no output
  options: no colour, no ASCII-only connectors, no width control.
- Validation cannot be narrowed. There is no per-rule and no per-severity
  filter, no way to turn a rule off, and no quiet or summary-only mode — use
  `--validate --json` and `jq` for that.
- `--validate` walks the store whole, so it cannot be combined with `--depth`
  and holds the whole node map in memory.
- `--json` builds the whole document before writing any of it, so peak memory
  grows with the number of nodes and an error part-way through a walk produces
  no document at all rather than a partial one.
- An argument beginning with `-` is always read as an option, so a store whose
  directory name starts with `-` cannot be inspected.
- Arguments are read with `std::env::args`, which panics on an argument that is
  not valid UTF-8. That happens before any argument validation, so the failure
  is a panic rather than the usual message and exit status 1.
- A directory that cannot be read ends the walk, and there is no way to skip
  past it.

## See also

- [Getting started](getting-started.md) — building the binary and a first
  store, as a tutorial.
- [Remote stores](remote-stores.md) — S3 credentials and regions, HTTP and
  WebDAV, static HTTP via consolidated metadata, and troubleshooting.
- [Architecture](architecture.md) — the `Store` trait, the walk, and how
  validation reuses it.
- [Zarr reference](zarr.md) — V2 and V3 layouts, sharding, consolidated
  metadata, and the degradation model.
- [OME-Zarr reference](ome-zarr.md) — recognition, versions, axes, multiscale
  datasets, plates and wells.
- [SpatialData reference](spatialdata.md) — elements, Parquet and AnnData
  payload summaries, and region linkage.
- [Project status](status.md) — the capability matrix and the explicit
  non-goals.
