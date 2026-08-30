# Contributing

Thanks for looking at `zarr-tree`. This file covers what you need to build it,
what the quality gate is, and which design constraints a change is expected to
preserve.

## Before you start

`zarr-tree` has a deliberately narrow scope: it reads Zarr metadata and prints
it. Its boundaries — no chunk reads, no writes, a short dependency list — are
design decisions rather than gaps, and a change that crosses one needs a reason
rather than an implementation.

For anything larger than a fix, it is worth reading first:

- [docs/status.md](docs/status.md) — what exists today, and what is deliberately
  absent.
- [docs/roadmap.md](docs/roadmap.md) — direction, and the boundaries that are
  not roadmap items.
- [docs/architecture.md](docs/architecture.md) — the layers and the invariants
  they rest on.

Opening an issue before a substantial change is useful — it may already be
tracked, or already have been ruled out. A typo fix, a doc correction or a
small bug fix needs no issue; just send the change.

## Development setup

You need Rust 1.88 or newer. That is the declared minimum supported version in
`Cargo.toml`, and CI checks it against the committed `Cargo.lock`. Edition 2024
alone needs only 1.85; `object_store` uses let-chains, which raises the
effective floor to 1.88.

```sh
git clone https://github.com/ronfinn/zarr-tree.git
cd zarr-tree
cargo check
cargo test
```

There is no `rust-toolchain.toml`, so your ambient toolchain is used.

[docs/getting-started.md](docs/getting-started.md) has the full installation
walkthrough and a first store to point the binary at.

## Building and testing

```sh
cargo check                  # type-check without linking; the fastest loop
cargo build                  # debug build -> target/debug/zarr-tree
cargo run -- <store>         # build and run against a store
cargo test                   # unit and CLI integration tests
cargo test <name>            # substring-match a test name
cargo test <name> -- --exact # exactly one test
cargo test -- --nocapture    # let tests print to stdout
```

## Quality gate

Run these before committing. CI runs the same checks, so a failure here is a
failure there.

```sh
cargo fmt
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
cargo +1.88.0 check --locked
git diff --check
```

What each one is for:

- `cargo fmt` applies the standard formatting. `cargo fmt --check` verifies it
  without modifying any file, which is the form CI runs — it exits non-zero if
  anything is unformatted rather than fixing it for you.
- `cargo check` type-checks the crate without linking a binary. It is the
  fastest way to find a compile error.
- `cargo clippy --all-targets -- -D warnings` runs Clippy over every target —
  the binary, the unit tests and the integration tests — and treats every
  warning as an error. Tests are included deliberately: lint regressions
  usually appear there first.
- `cargo test` runs the unit tests and the CLI integration tests.
- `cargo +1.88.0 check --locked` verifies the declared minimum Rust version.
  `--locked` means the committed `Cargo.lock` is what gets checked, not
  whatever a fresh resolve would pick, so a dependency that quietly raised its
  own MSRV is caught. Install the toolchain once with
  `rustup toolchain install 1.88.0`.
- `git diff --check` catches trailing whitespace and other whitespace errors
  before they reach a commit.

## Design constraints

These are the invariants the implementation rests on. A change to functionality
should preserve them unless it is deliberately changing the direction of the
project, in which case say so explicitly. [docs/architecture.md](docs/architecture.md)
gives the reasoning behind each.

- **Read-only.** Nothing is ever opened for writing. There is no repair mode,
  no migration and no consolidation writer.
- **Metadata-first.** Only metadata documents are read. Chunk data, pixel
  values, Parquet records and expression matrices are not.
- **Storage-neutral semantic parsing.** Traversal, interpretation and both
  renderers know nothing about where the bytes came from. Only the `Store`
  trait and its implementations do. A format reader that reaches for a
  filesystem path has broken this.
- **Local and remote parity.** The same store produces the same tree whether it
  is a directory, an S3 prefix or an HTTP URL. A feature that works only
  locally is incomplete.
- **Arrays are leaves.** Traversal stops at an array, so its chunk objects are
  never listed. This is what makes a remote walk affordable, and it must stay
  true.
- **No chunk enumeration.** Related, and stronger: nothing anywhere lists the
  keys under an array.
- **Graceful degradation.** Malformed metadata costs one field or one node's
  label — an unreadable field prints `?`, an unreadable node prints
  `[unknown]` — and never the rest of the walk. New readers should degrade the
  same way rather than aborting.
- **Consolidated metadata is a snapshot.** `ConsolidatedStore` is an overlay
  `Store`, so every reader above it gets the JSON it always got. It is
  opportunistic — no store that worked without it may come to depend on it —
  and all-or-nothing, so a tree is never half snapshot and half live. A stale
  snapshot is reported as it stands; it is not checked against the store.
- **Parquet rows are not read.** Only the footer, and only through a bounded
  suffix read. No record, page or row group is decoded.
- **AnnData values are not read.** A table summary comes out of Zarr metadata
  alone. Nothing is counted, no category is decoded, no index label is read.
- **Validation reuses inspection metadata.** `--validate` checks the structure
  a store declares against the store itself, using metadata the tree already
  reads. It is not schema conformance, and there is no rule registry or policy
  engine.

Two more that are about the shape of the project rather than the code:

- The dependency list is `serde_json`, `object_store` (`aws` and `http` only),
  `parquet` (no default features), `tokio` (current-thread) and `url`. Adding
  to it needs a real justification.
- The default output is inspection, not validation. Unfamiliar values are
  printed as stored. `--validate` is the one exception.

## Making a change

1. Make the smallest coherent change — one idea per commit.
2. Add or update tests for it.
3. Update any documentation the change affects, including
   [docs/status.md](docs/status.md) if it changes what the tool can do.
4. Run the quality gate above.
5. Read `git diff` before committing. It catches more than the linters do.
6. Open a pull request.

Commit messages in this repository tend to use a short prefix — `feat:`,
`fix:`, `docs:`, `test:`, `chore:` — followed by an imperative summary and a
paragraph on why. That is a convention worth matching, not a requirement, and
nothing enforces it.

There is no branch naming convention. Work on whatever branch name suits you.

## Tests

The layout is flat and intentional:

- **Unit tests** live in `src/main.rs`, alongside the code. They cover metadata
  parsing directly — feed a reader some JSON, assert on what comes out.
- **CLI integration tests** live in `tests/cli.rs`. They run the compiled binary
  against throwaway fixture stores built in a temporary directory, and assert
  on what it prints and what it exits with.

`cargo test` runs both.

Some guidance on what to test with:

- **Prefer small synthetic fixtures.** Most behaviour is a question about a
  metadata document, and a hand-written `.zarray` or `zarr.json` of a few lines
  is a better test than a real store: it is fast, it is readable, and it can be
  made malformed on purpose. The Parquet fixtures are written by the same crate
  that reads them back, so those tests still run against real Parquet bytes
  with a real footer.
- **Test remote behaviour against something small and local.** A test that
  exercises HTTP handling should stand up a minimal local server rather than
  reaching for the network. A test whose actual subject is a real remote
  service's behaviour is the exception, not the default.
- **Do not download large public datasets.** Nothing in the test suite should
  fetch a multi-gigabyte store. When a real public store is genuinely the
  subject, use the smallest relevant one and keep it out of the default test
  run.

## Documentation

Each document has a job, and keeping them that way is what stops the README
growing back into everything:

| File | Owns |
| --- | --- |
| `README.md` | Landing page: what it is, a quick start, where to go next. |
| `docs/getting-started.md` | Tutorial — install it, run it on a first store. |
| `docs/cli.md` | Command reference: options, JSON fields, validation rules, exit statuses. |
| `docs/remote-stores.md` | S3, HTTP, WebDAV, credentials, troubleshooting. |
| `docs/architecture.md` | Internals and design rationale. |
| `docs/zarr.md` | Zarr V2 and V3 layouts, arrays, sharding, consolidation. |
| `docs/ome-zarr.md` | OME-Zarr recognition, versions, axes, multiscales, HCS. |
| `docs/spatialdata.md` | SpatialData elements, Parquet payloads, AnnData tables. |
| `docs/status.md` | Capability matrix — what is implemented today. |
| `docs/roadmap.md` | Direction, with nothing promised. |
| `CHANGELOG.md` | User-visible changes, per release. |

If a change adds a capability, it belongs in `docs/status.md` and in the
relevant format or CLI reference — not as another section in the README.

## Pull requests

Keep the description short and factual. Say:

- what changed;
- why;
- what you ran to verify it;
- what documentation you updated, or that none was affected;
- anything that changes existing output or exit statuses, since that is what
  breaks somebody's script.

No screenshots — it is a CLI. Nothing needs a version bump, a tag or a release;
that is maintainer work, and a PR that does it will just have to be undone.
Adding a `CHANGELOG.md` entry is welcome for a user-visible change and pointless
for a typo.

## Reporting bugs and proposing features

Bugs and feature requests go to
[GitHub Issues](https://github.com/ronfinn/zarr-tree/issues). There is a form
for each. The most useful bug report includes the exact command, the output you
got, the output you expected, and the smallest store structure or metadata
document that reproduces it.

Please do not attach credentials, signed URLs, tokens or private dataset
content to an issue, and do not upload scientific data — a few lines of the
metadata document is what is needed.

Security vulnerabilities do not go to Issues. See [SECURITY.md](SECURITY.md).
