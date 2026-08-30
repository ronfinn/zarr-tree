# Remote stores

`zarr-tree` reads a store on S3 or an HTTP server exactly as it reads one on
disk. The storage backend changes; the walk, the tree, the options and the JSON
do not. Traversal and interpretation are storage-neutral — only the `Store`
trait and its two implementations know where the bytes come from — so anything
the [README](../README.md) says about output applies unchanged to a remote
store.

The scheme of the store argument is the only thing that decides:

```sh
zarr-tree /data/example.zarr              # local directory
zarr-tree s3://bucket/path/example.zarr   # S3
zarr-tree https://example.org/example.zarr # HTTP(S)
```

Every other argument is a path on this machine, including a relative path that
happens to contain `s3://` somewhere after its start.

Access is read-only in every backend. Nothing is written, and no chunk object is
listed or fetched.

## Amazon S3

```
$ zarr-tree --depth 1 s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr
s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr [group]
├── mapping [group]
├── recon-1 [group]
└── recon-2 [group]
```

`--depth`, `--json` and `--validate` behave as they do locally.

Per node, a walk costs one `ListObjectsV2` with `delimiter=/` for a group, and
one to three `GetObject` calls to classify it — `.zgroup`, then `.zarray`, then
`zarr.json`, stopping at the first that answers. A Zarr V3 store therefore pays
two misses per node.

An array is a leaf on S3 exactly as it is on disk, and that is what makes remote
traversal affordable: a listing is made only for a group, so an array's chunk
objects are never enumerated, however many millions of them there are. This is
current behaviour and a design rule the project intends to keep — see
[Explicit non-goals](status.md#explicit-non-goals).

Errors name the category and nothing more:

```
$ zarr-tree s3://janelia-cosem-datasets/nope.zarr
error: no such bucket or prefix: s3://janelia-cosem-datasets/nope.zarr

$ zarr-tree s3://
error: expected s3://bucket/prefix, not "s3://"
usage: zarr-tree [OPTIONS] <STORE>
```

A failed request never prints a key, token or signature, and a multi-line XML
response is cut to its first line.

## AWS credentials

Settings come from the usual `AWS_*` environment variables and nothing else.
There is no login, no profile manager, no credential file of `zarr-tree`'s own,
and no `--profile`, `--region` or `--endpoint` flag: the environment is the
whole interface.

| Variable | Effect |
| --- | --- |
| `AWS_ACCESS_KEY_ID` | Access key. |
| `AWS_SECRET_ACCESS_KEY` | Secret key. |
| `AWS_SESSION_TOKEN` | Session token for temporary credentials. |
| `AWS_REGION` | Bucket region. Without it, `us-east-1` is assumed. |
| `AWS_ENDPOINT_URL` | Alternative S3-compatible endpoint. |
| `AWS_WEB_IDENTITY_TOKEN_FILE` | Web-identity credentials. |
| `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` | Container credential endpoint. |
| `AWS_CONTAINER_CREDENTIALS_FULL_URI` | Container credential endpoint. |
| `AWS_SKIP_SIGNATURE` | Overrides the anonymous default in either direction. |

**Requests are unsigned by default.** When none of the six credential variables
above names a credential, `zarr-tree` sends requests unsigned. That is what a
public bucket wants, and it is what the Janelia example above relies on. The
reason is practical: the underlying credential chain ends at the EC2 instance
metadata service, and off an EC2 instance every request would spend a second
failing to reach `169.254.169.254` before returning a signature error.

Set `AWS_SKIP_SIGNATURE=false` to force the credential chain instead. On an EC2
instance with an instance role, that is what you want:

```sh
AWS_SKIP_SIGNATURE=false zarr-tree s3://my-bucket/path/store.zarr
```

**Named profiles are not read.** `object_store` does not read
`~/.aws/credentials` or `~/.aws/config`, so a named profile has no effect on its
own — setting `AWS_PROFILE` does nothing here. The AWS CLI bridges the gap by
exporting a profile's credentials as environment variables:

```sh
eval "$(aws configure export-credentials --profile my-profile --format env)"
zarr-tree s3://my-bucket/path/store.zarr
```

That sets `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and, for a temporary
credential, `AWS_SESSION_TOKEN` in the shell — which is enough for the
credential chain to pick them up, and enough to switch off the anonymous
default.

Never put a credential on a command line you will commit or paste.

## AWS region

Without `AWS_REGION`, the bucket is assumed to be in `us-east-1`. Nothing
discovers a bucket's region: S3 answers a request sent to the wrong region with
a redirect, which is reported as a failed request rather than followed.

```sh
AWS_REGION=eu-west-1 zarr-tree s3://my-bucket/path/store.zarr
```

## Custom S3 endpoints

`AWS_ENDPOINT_URL` points the S3 backend at a different host, which is how an
S3-compatible service that is not AWS is reached. The bucket name stays the
first path segment of the `s3://` URI.

The EBI Embassy object store hosting the IDR OME-Zarr collection is one such
service, and it is public, so the anonymous default applies:

```sh
AWS_ENDPOINT_URL=https://uk1s3.embassy.ebi.ac.uk \
  zarr-tree --depth 1 s3://idr/zarr/v0.4/idr0062A/6001240.zarr
```

This is the generic endpoint override `object_store` provides, so any service
that speaks the S3 API on a single endpoint — MinIO and Ceph RGW deployments
included — should work through it. `zarr-tree` does not test against those and
makes no compatibility claim beyond "the endpoint variable is passed through".

Some non-AWS endpoints also need `AWS_ALLOW_HTTP=true` for a cleartext `http://`
endpoint, and a region value even where the service ignores it.

## Public and anonymous buckets

A public bucket needs no configuration at all — the anonymous default is exactly
right for it:

```sh
zarr-tree s3://janelia-cosem-datasets/jrc_cos7-11/jrc_cos7-11.zarr
```

The distinction is worth keeping clear:

| | Public / anonymous | Authenticated |
| --- | --- | --- |
| Credential variables | none set | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`, or web identity, or container, or instance role |
| Signing | requests sent unsigned | requests signed by the credential chain |
| What switches it | the default | any credential variable, or `AWS_SKIP_SIGNATURE=false` |

Setting a credential for a public bucket is not harmless: a signed request
against a bucket your credential has no policy for is a `permission denied`,
where an unsigned one would have succeeded.

## HTTP and HTTPS

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

The crucial distinction for HTTP is between reading metadata and discovering
children:

| Operation | How it is done | What it needs |
| --- | --- | --- |
| Read a node's metadata | ordinary `GET` | any HTTP server |
| Find a node's children | WebDAV `PROPFIND`, `Depth: 1` | a WebDAV-capable server, **or** consolidated metadata |

So a full tree over HTTP needs *either* a server that answers `PROPFIND` *or* a
store carrying consolidated metadata. With neither, only `--depth 0` works.

The URL is the store root and internal paths resolve beneath it. A trailing
slash makes no difference, and percent-escapes are decoded and re-applied per
path segment rather than pasted together. A query string is kept and sent with
every request, which is the one shape of access token a static server tends to
want. `http://` is allowed only because the URI asked for it in so many words.

HTTP access is anonymous. There is no `Authorization` header, no cookie, no
`--user` and no client certificate.

## WebDAV listing

Plain HTTP has no standard "list this directory" operation, so `zarr-tree` asks
with a WebDAV `PROPFIND`. A server configured for WebDAV — Apache `mod_dav`,
nginx `ngx_http_dav_module` with `PROPFIND`, ownCloud/Nextcloud, and most object
gateways offering a DAV endpoint — gives a full tree.

An ordinary static file server does not, and is told apart from a missing store,
because saying "not found" about a URL whose metadata has just been read would
be wrong:

```
$ zarr-tree --depth 1 https://static.example/data/example.zarr
https://static.example/data/example.zarr [group, OME-Zarr 0.4]
error: cannot list https://static.example/data/example.zarr: the server answers
GET but not the WebDAV listing needed to find child nodes
```

Such a store can still be inspected one node at a time, since `--depth 0` needs
no listing at all:

```
$ zarr-tree --depth 0 https://static.example/data/example.zarr
https://static.example/data/example.zarr [group, OME-Zarr 0.4]
├─ axes: c, z, y, x
├─ pyramid levels: 3
└─ datasets: 0, 1, 2
```

Directory-index pages are never scraped: an HTML listing is a page for people,
not a protocol, and reading one would mean guessing at a server's theme.

One cost to know about: a `PROPFIND` per group becomes two requests per group on
a server that redirects a collection URL to its trailing-slash form, as Apache
`mod_dav` does.

## Static HTTP and consolidated metadata

A store carrying consolidated metadata needs no listing at any depth, and a
plain static server therefore serves it in full. Two forms are read, the two
that current zarr-python writes:

| Zarr version | Document | Accepted form |
| --- | --- | --- |
| V2 | `.zmetadata` at the store root | `zarr_consolidated_format` 1 |
| V3 | a `consolidated_metadata` block in the root `zarr.json` | `kind: inline`, `must_understand: false` |

Either document holds a copy of every metadata file in the tree, keyed by path,
so the hierarchy can be reconstructed from it without a single directory
listing. The format detail — the accepted documents, what is filtered out of
them, and how the overlay behaves — is in the
[Zarr reference](zarr.md#consolidated-metadata).

```
$ zarr-tree --depth 1 https://ncsa.osn.xsede.org/Pangeo/pangeo-forge/gpcp-feedstock/gpcp.zarr
https://ncsa.osn.xsede.org/Pangeo/pangeo-forge/gpcp-feedstock/gpcp.zarr [group]
├── lat_bounds [array]
│   ├─ shape:  [180, 2]
│   ├─ chunks: [180, 2]
│   └─ dtype:  <f4
...
└── time_bounds [array]
    ├─ shape:  [9226, 2]
    ├─ chunks: [200, 2]
    └─ dtype:  <i8
```

That server answers `PROPFIND` with `405 Method Not Allowed`. The whole tree
above came out of one `GET` of `.zmetadata`.

Two properties matter when reading a remote store this way:

- **Opportunistic.** A store with no consolidated metadata, or with a form not
  read here, is walked as it would have been anyway — listings on S3, `PROPFIND`
  over HTTP. Nothing that worked without consolidation comes to depend on it.
- **All-or-nothing.** Once the document has been read, the store itself is not
  consulted again for the Zarr walk, so a tree is never half snapshot and half
  live. Consolidated metadata is a snapshot and may be stale; `zarr-tree`
  reports it as it stands and does not check it against the store, which would
  cost exactly the requests consolidation exists to avoid.

`.zmetadata` is looked for first, as V2 is everywhere else here, so a
consolidated V2 store costs one request at any depth and a consolidated V3 store
costs two.

## Remote Parquet payloads

A SpatialData points or shapes element keeps its data in Parquet beside the
element's metadata, and `zarr-tree` summarises it from the **file footer alone**
— no row group, page, record or value is ever read. See
[Payload files](../README.md#payload-files) for what the summary contains.

Remotely this uses bounded range reads rather than a download. The current
footer window is 64 KiB: one `HEAD` for the object size, then one range `GET` of
the last 64 KiB, and — only if the footer is larger than that window — one
further range read of exactly the right size. Two reads at worst, whether the
file is three kilobytes or two gigabytes.

Two consequences:

- A file smaller than the footer window is returned in full, because the range
  is clamped to the start of the object. For a small `shapes.parquet` that is
  the whole file; it is still one bounded request.
- A large payload is not downloaded. Observed on one fixture, a 77 MB
  transcripts payload cost one `HEAD` and one 64 KiB range `GET`. That is an
  observation about that file, not a guarantee about yours — what is guaranteed
  is the request shape, not a byte count.

The two payload kinds differ in what the backend must support:

| Element | Payload path | Needs a listing? |
| --- | --- | --- |
| shapes | `shapes.parquet`, a single file | No — the name is known, so a static HTTP server serves it |
| points | `points.parquet/`, a directory of `part.N.parquet` | Yes — the part filenames are never guessed at |

So on a listing-less static HTTP server a points payload that exists but cannot
be enumerated prints one marker and no more:

```
$ zarr-tree --depth 1 https://static.example/data/xenium.zarr
https://static.example/data/xenium.zarr [group, SpatialData 0.2]
└── points [group]
    └── transcripts [group, SpatialData points]
        └─ parquet files: ?
```

`parquet files: ?` means the payload is there and could not be read; `--json`
carries `"parquet": null` for the same case. A payload that is genuinely absent
prints nothing at all. Under `--validate` this is a `WARN`, not an `ERROR`.

## Remote AnnData summaries

A SpatialData table summary is built from Zarr metadata paths — `obs`, `var`,
each dataframe's index array, and `X` — which is five metadata reads and no
listing. No expression value is read and no chunk is opened, so a table costs
the same remotely as locally, and on a consolidated store those five reads come
wholly out of the snapshot with no request of their own.

## Remote efficiency

The design principles, rather than benchmarks:

- A group costs its metadata reads and, where the backend needs one, a single
  hierarchy listing.
- An array terminates traversal. Chunk keys are never listed and chunk data is
  never fetched — the single largest reason a remote walk is affordable.
- A node is classified with up to three `GetObject`/`GET` calls (`.zgroup`,
  `.zarray`, `zarr.json`), so a Zarr V3 store pays two misses per node.
- Consolidated metadata replaces every one of those reads and every listing with
  one or two requests for the whole store.
- Parquet summaries use bounded suffix reads, never a download.
- `--depth 0` reads no listing at all, which makes it cheap even on a store with
  a million chunk objects.
- There is no concurrency and no caching. Requests go out one at a time, and
  nothing is kept between runs.

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `permission denied: s3://…` | Credentials lack a policy for the bucket — or a credential is set for a public bucket that wanted an unsigned request |
| `authentication failed: s3://…` | The credential in the environment is not valid |
| `no such bucket or prefix: s3://…` | Wrong URI, or the right URI against the wrong endpoint |
| `request failed: …` mentioning a redirect | Bucket is not in `us-east-1`; set `AWS_REGION` |
| A named AWS profile appears to be ignored | It is: `~/.aws/credentials` is not read. Use `aws configure export-credentials --format env` |
| `cannot list …: the server answers GET but not the WebDAV listing needed to find child nodes` | Static HTTP with no `PROPFIND` and no usable consolidated metadata. Use `--depth 0`, or a consolidated store |
| `not found: https://…` | The URL is wrong, or the store root is not where the URL points |
| A points element shows `parquet files: ?` | The payload is there but could not be listed or its footer could not be read — commonly a listing-less HTTP server |
| An institutional S3 URL fails immediately | The service is not AWS; set `AWS_ENDPOINT_URL` |
| A node reads `[unknown]` remotely | A remote read that fails mid-walk is indistinguishable from a missing file. Only the root is checked properly, before anything is printed |

## Current limitations

Remote-specific. The full list is under
[Limitations](../README.md#limitations).

- S3 and HTTP(S) only. No GCS, no Azure, no ZIP store, no writing of any kind.
- `~/.aws/credentials` and `~/.aws/config` are not read, so named profiles have
  no effect on their own, and there is no `--profile`, `--region` or
  `--endpoint` flag.
- No S3 region discovery. A bucket outside `us-east-1` needs `AWS_REGION`.
- HTTP access is anonymous: no `Authorization` header, no cookie, no client
  certificate. A query string on the URL is passed through, and that is all.
- A GET-only HTTP server needs consolidated metadata for a full walk; without
  it, only `--depth 0` works. HTML directory-index pages are deliberately not
  scraped.
- Only the two consolidation forms current zarr-python writes are read, and a
  stale snapshot is reported as it stands.
- A points Parquet payload cannot be enumerated on a listing-less server, and
  its part filenames are never guessed at.
- No concurrency and no caching: requests go out one at a time, and nothing is
  kept between runs.
- Everything is read-only.

## See also

- [Getting started](getting-started.md) — build, first store, the option set.
- [Zarr reference](zarr.md) — what is read from each metadata layout, and the
  consolidated metadata formats these servers serve.
- [Architecture](architecture.md) — the `Store` trait and the consolidated
  overlay.
- [Project status](status.md) — the capability matrix.
- [README](../README.md) — the full reference.
