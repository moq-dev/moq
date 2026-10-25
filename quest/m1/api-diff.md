# [L] API-diff check on release PRs

## Goal

A `just` recipe diffs each published package's public API against its last
published version, and every PR into `release` runs it. A backport fails on
any break; the breaking cut PR reports the diff for the
[migration audit](/quest/m2/release/migrate-audit.md).

## Plan

Decided 2026-10-16 (maintainer interview), split out of the migration audit
and moved to m1 on 2026-10-22 (maintainer, from an external API audit) so
the report runs locally against `main` while m1's breaks are still landing,
instead of first appearing at the cut:

- Commit `!` markers undercount: a 2026-10-16 sample estimated 12-15
  unmarked breaks on top of 87 marked ones, a floor since signature-only
  changes escape the heuristic. This check is the backstop; PRs into `main`
  get no CI check.
- Runs on PRs into `release` only: backports must keep the published API
  (CONTRIBUTING), and the cut PR gets a full audit.
- Backports fail on any detected break. The cut PR only reports: Rust
  versions are bumped by release-plz after the cut lands, so the diff cannot
  gate on a version bump there.
- One `just` recipe, three backends: cargo-semver-checks for Rust crates,
  emitted `.d.ts` API reports for `@moq/*` packages, and a `moq.h` plus
  UniFFI interface diff for the bindings. Prefer maintained tools over
  hand-rolled diffing. Wire it into the workflow that runs on PRs into
  `release`.
- Baseline is the last published artifact, not a git tag: `@moq/*` and
  `moq-rs` publish without tags. Rust compares against crates.io
  (cargo-semver-checks' default), JS against the npm tarball's declarations,
  and the FFI interfaces against the published `moq-ffi` crate and the last
  released C archive's `moq.h`.
- Coverage: the report covers Rust crates, `@moq/*` declarations, and the
  generated FFI interfaces. A break only in a hand-written binding wrapper
  (`py/moq-rs`, `kt/moq`, `dart/moq`, `go/wrapper`, `swift`, `cpp/moq`,
  `cpp/obs`) or a binary's flags, env vars, or config keys escapes it; the
  migration audit reads those by hand. Per-language wrapper checks are out
  of scope until a missed break shows they are needed.

Run it against `main` when it lands and put the unmarked breaks it finds in
the PR description, so the migration audit covers them at the cut.
