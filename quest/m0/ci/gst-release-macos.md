# [S] moq-gst releases publish again

## Goal

A `moq-gst-v*` tag's run passes its `Build tarball (aarch64-apple-darwin)`
job, so its `Release` job publishes the GitHub release, and the next moq-gst
release appears under the repository's releases. The fix addresses why
`brew test` hangs, without raising its timeout or skipping the test.

## Plan

Found 2026-11-06 (/quest-unblock) while re-running
[moq-dev/moq#7936](https://github.com/moq-dev/moq/pull/7936)'s failed tag runs.
Every moq-gst tag since 0.4.20 failed the same way:
[0.4.21](https://github.com/moq-dev/moq/actions/runs/41573749359),
[0.4.22](https://github.com/moq-dev/moq/actions/runs/41711099214), and
[0.4.23](https://github.com/moq-dev/moq/actions/runs/42129723974) (also on
its 2026-11-06 re-run). The `Test Homebrew formula against tarball` step
(`just gst-formula`, `sh/gh/gst-formula.test.py`) runs
`brew test moq-dev/ci-gst/moq-gst`. `gst-inspect-1.0` logs LaunchServices
and `com.apple.hiservices-xpcservice` "Connection invalid" errors, then the
test hits Homebrew's 5-minute timeout. The `Release` job is skipped, so those
three GitHub releases never published; their crates did.

Nightly's same job passes on `main` (2026-11-06), so start by comparing what
differs on `release`: the workflow and Just recipe, the formula, the GStreamer
or Homebrew versions it installs, and the plugin build. Reproduce on a macOS
runner before changing anything.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ plan it in m0, and publish
the next release rather than backfilling 0.4.21 to 0.4.23. Not chosen:
backfilling each missed version; m3 with the post-cut release work.

Public API: none. Wire: none.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - its secret cleanup waits on release tag runs passing, and this job is the one still failing
