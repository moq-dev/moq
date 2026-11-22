# [S] Queue simultaneous apt/rpm/brew releases and retry npm CDN propagation

## Goal

Simultaneous `moq-cli`, `moq-relay`, and `moq-gst` release tags all reach apt,
rpm, and Homebrew without cancelling each other, and `bun --filter '*' release`
does not fail on npm CDN propagation lag between newly published sibling
`@moq/*` packages.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place the cut-blocking release-pipeline bugs in `m0`
and gate [the release cut](/quest/m3/release/cut.md) on them (rejected: putting
all pipeline fixes in `m1` without gating the cut; fixing only the 3
publish-breaking items and dropping the rest).

1. **Queue concurrent package-repo publishers instead of cancelling them.**
   - `.github/workflows/apt-repo.yml:23-25`, `.github/workflows/rpm-repo.yml:21-23`,
     and `.github/workflows/release-brew.yml:28-30` set a shared `concurrency`
     group with `cancel-in-progress: false` and no `queue`. GitHub Actions only
     holds one pending run per group, so when a release cut fires `moq-cli`,
     `moq-relay`, and `moq-gst` tags together, the third run silently cancels
     the second pending run, and `.github/workflows/alert.yml:62-71` ignores
     `cancelled` conclusions.
   - Fix: switch those workflows to queued concurrency (`queue: max` or
     per-tag/serialized dispatch compatible with GitHub Actions; verify GitHub
     Actions syntax and ensure a release-cut batch of 3+ tags serializes every
     run without cancellation). Also review `.github/workflows/release-kt-ffi.yml:20-22`
     and `.github/workflows/release-swift-ffi.yml:22-24` so tag-triggered
     releases cannot collide on the same concurrency group.
   - Verify whether `.github/workflows/alert.yml` should also alert on
     unexpected `cancelled` runs for release workflows.

2. **Retry `npm view` CDN propagation in `js/common/release.ts`.**
   - `js/common/release.ts:75-106` checks whether a package version exists on
     npm and resolves sibling `@moq/*` workspace specs via `npm view`. During
     `bun --filter '*' release`, a dependent package runs immediately after its
     sibling publishes, and npm's registry CDN can return `E404` or stale
     metadata for tens of seconds, causing the publish to fail mid-wave.
   - Fix: when `!dryRun`, retry `E404` / empty `npm view` results for newly
     published sibling `@moq/*` packages with bounded backoff (60 to 90 s total)
     before failing.

Public API: none. Wire: none. Cover `js/common/release.ts`'s retry helper with
unit tests covering transient `E404`, eventual success, and timeout; validate
workflow YAML syntax and concurrency semantics.
