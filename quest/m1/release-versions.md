# [M] Every package's release backport ships

## Goal

A fix merged into `release` for any published package publishes without a
hand-made version bump. Today release-plz bumps only Rust crates, so a
backport touching only a JS, Python, Dart, Kotlin, Swift, Go, C, or C++
package lands on `release` unchanged in version and its release workflow
skips it as already published.

## Plan

Found 2026-10-28: the @moq/flate and @moq/net backports
([moq-dev/moq#6961](https://github.com/moq-dev/moq/pull/6961),
[moq-dev/moq#6959](https://github.com/moq-dev/moq/pull/6959),
[moq-dev/moq#6967](https://github.com/moq-dev/moq/pull/6967)) merged and the
release PR merged, but Release JS logged "already published, skipping" and a
separate hand bump was needed.

Decided 2026-10-28 (maintainer):

- Scope: every hand-versioned package listed in CONTRIBUTING.md's Versions
  section, not just JS.
- Mechanism: a release-plz-like bot. On each push to `release`, it opens or
  updates one PR that patch-bumps every package whose sources changed since
  its last published version (with the matching lockfile entries, e.g.
  `bun.lock`, `uv.lock`); merging it publishes through the existing release
  workflows. Prefer an existing maintained tool if one covers these
  ecosystems; extending release-plz's own PR is also fine.
- Until this lands, a backport author bumps by hand.

Update CONTRIBUTING.md's Versions and backport bullets to describe the result.

Public API: none. Wire: none.
