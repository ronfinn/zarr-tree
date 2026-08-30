# Roadmap

The roadmap is directional. It is not a promise of dates, releases or version
numbers, and nothing here is committed until it is merged. For what actually
exists today, see [Project status](status.md).

Items move down this page as often as up it. An item under *Research* may turn
out to be a bad idea and simply be dropped.

## Near term

Work that is intended, small, and consistent with the current design.

- Final repository and release polish. The README has been split into focused
  guides under `docs/`, and contributor and maintenance documents are in place;
  what remains is tidying up around a release.
- Expanding metadata-only structural validation, within the existing model —
  findings over metadata already read, no schema and no rule engine.
- Improving how OME-Zarr metadata is presented.
- Showing a node's user attributes on request.
- Small usability fixes that come out of running the tool against real public
  stores.

## Under consideration

Plausible, not planned. Each would need to earn its place against the design
rules in `CLAUDE.md` — metadata only, no chunk reads, a short dependency list.

- OME-Zarr `image-label` metadata beyond its presence.
- Channel metadata and `omero` summaries.
- Coordinate transformations and physical scales.
- More detailed HCS metadata: acquisitions, fields of view.
- Natural ordering for numbered hierarchy names, so `10` sorts after `9`.
- Distribution through crates.io.
- Pre-built release binaries.

## Research

Open questions. No design exists for any of these, and pursuing one may well
show that it does not belong in this tool.

- [`zarrs`](https://github.com/LDeakin/zarrs) integration, if an actual
  array-reading, chunk-decoding or remote-store need ever justifies the
  dependency.
- Chunk-aware inspection — reporting on chunk layout without reading chunk
  data.
- Selective array reads, for the cases where a value genuinely answers a
  structural question.
- Remote concurrency and performance work: requests currently go out one at a
  time and nothing is cached between runs.
- Additional object-store backends.

## Not on the roadmap

The boundaries under [Explicit non-goals](status.md#explicit-non-goals) are not
roadmap items. They are decisions.
