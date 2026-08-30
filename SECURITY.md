# Security policy

`zarr-tree` is a small, read-only command-line tool maintained by one person.
This file says which versions get fixes, how to report a vulnerability, what is
in scope, and how a fix is disclosed.

## Supported versions

Security fixes are made against the current development branch (`master`) and,
where appropriate, included in the next release. Older releases are not
patched, and there are no long-term support branches.

The latest release is v0.3.0. See
[Releases](https://github.com/ronfinn/zarr-tree/releases) for the current one at
any time.

No response-time commitment is made, and no CVE will be requested on your
behalf. If a report warrants a CVE, that is something you or a coordinating
body would pursue.

## Reporting a vulnerability

**Please do not open a public issue describing a vulnerability**, and do not
include exploit details, reproduction steps or affected store contents in any
public thread.

### Current reporting route

GitHub's private vulnerability reporting is **not currently enabled** on this
repository, and there is no published maintainer contact address. That means
there is, at the time of writing, no private reporting channel — a known
repository-maintenance gap rather than a deliberate policy, and one that should
be closed by enabling private vulnerability reporting under
*Settings → Code security*.

Until it is, the safe route is:

1. Open an issue with a subject such as "security contact request", containing
   **no technical detail** — no description of the flaw, no reproduction, no
   input that triggers it. Say only that you have a security report and are
   asking for a private channel.
2. The maintainer will arrange a private channel and follow up there.

If the repository's **Security → Report a vulnerability** button is visible to
you when you read this, private reporting has since been enabled and that is
the preferred route; use it instead of step 1.

### What to include, once a private channel exists

- The version or commit affected.
- The command and store shape that triggers it.
- The behaviour observed, and why you believe it is a security issue.
- The smallest metadata document that reproduces it — not a dataset.

Never send credentials, signed URLs, tokens or private dataset content.

## Scope

`zarr-tree` reads metadata from stores it did not create, which is the source
of most of its realistic exposure. Reports in these areas are in scope:

- **Malicious or malformed metadata.** A crafted `zarr.json`, `.zarray`,
  `.zgroup`, `.zmetadata` or OME/SpatialData attribute block that causes a
  crash, a hang, an out-of-bounds read, or unbounded memory growth.
- **Malicious Parquet footers.** A crafted footer that causes the same, in a
  points or shapes payload summary.
- **Untrusted remote input.** Anything an HTTP or S3 endpoint can return —
  including a hostile or compromised server — that leads to a crash, a hang, or
  a request going somewhere it should not.
- **Path handling.** A store path, key or declared child name that escapes the
  store root, reads a file outside it, or is otherwise mishandled — including
  path separators, `..` segments and non-UTF-8 names.
- **Resource exhaustion.** Metadata structures that are legal but
  unexpectedly large or deeply nested, and cause unbounded memory or CPU use
  from a single small document.
- **Credential handling.** Anything that causes AWS credentials taken from the
  environment to be written to disk, printed in output or an error, or sent to
  an unintended endpoint.

### Scope boundaries

Some classes of problem are absent by construction rather than by care:

- `zarr-tree` is read-only. It opens nothing for writing, and modifies no store.
- It does not execute code from a store. Metadata is parsed as JSON and Parquet
  footers as thrift; nothing in a store is evaluated, deserialised into a
  callable, or run.
- It does not decode chunk data. Arrays are leaves, so no codec is invoked on
  store content and no chunk object is even listed.
- It does not write credentials anywhere. Credentials are read from the
  environment by `object_store` and used for signing; nothing persists them.

This narrows the attack surface. It does not make the tool secure, and it says
nothing about the parsers it does run — a crafted document reaching
`serde_json` or the `parquet` footer reader is exactly the sort of thing worth
reporting.

Out of scope: vulnerabilities in Rust, in a dependency, or in a remote object
store, unless `zarr-tree` uses them in a way that makes an otherwise
unexploitable issue exploitable. Report those upstream.

## Disclosure

Once a private channel is established:

- The report is acknowledged, and confirmed or explained as not a
  vulnerability.
- A fix is developed on `master` with a regression test.
- The fix is released, and the issue is described in `CHANGELOG.md` and the
  release notes.
- The reporter is credited unless they ask not to be.

Please give the fix a chance to land before publishing details. There is no
fixed embargo period, and no expectation that you wait indefinitely.
