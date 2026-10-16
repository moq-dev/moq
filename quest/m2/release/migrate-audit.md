# [L] API-diff check on release PRs

## Goal

Every PR into `release` runs a check that diffs each published package's
public API against its last release tag. A backport fails on any break; on
the breaking cut PR the diff is reported for the auditor, who confirms every
break has its `MIGRATE.md` entry before the cut merges.

## Plan

Decided 2026-10-16 (maintainer interview):

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
  `release`, and add the audit step to CONTRIBUTING.md's Releases section.
- For this cut, fill every gap the report shows in the matching `MIGRATE.md`.
- Required by the cut, after m1's breaking work has landed.

## Required

- [Breaking release readiness](/quest/m1/README.md) - all breaking work and the MIGRATE.md convention have landed
