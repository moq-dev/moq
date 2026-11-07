# [XS] moq-gst releases publish again

## Goal

`release` carries `main`'s Homebrew formula test, so a `moq-gst-v*` tag's
`Build tarball (aarch64-apple-darwin)` job passes, its `Release` job publishes
the GitHub release, and the next moq-gst release appears under the
repository's releases.

## Plan

The backport is [moq-dev/moq#8109](https://github.com/moq-dev/moq/pull/8109). It brings
[moq-dev/moq#5841](https://github.com/moq-dev/moq/pull/5841)'s
`.github/homebrew/Formula/moq-gst.rb.tmpl` test to `release`, so the test no longer runs
`gst-inspect-1.0` inside Homebrew's sandbox, where it hangs on a headless Mac. All that
remains is proving it with the next moq-gst tag. Decided 2026-11-06 (maintainer): publish the
next release rather than backfilling 0.4.21 to 0.4.23.

## Waiting

- Maintainer: merge [moq-dev/moq#8109](https://github.com/moq-dev/moq/pull/8109), then cut the next `moq-gst-v*` tag (agents do not push tags or dispatch release workflows). Done when that tag's `Build tarball (aarch64-apple-darwin)` job passes and its GitHub release publishes; then delete this quest.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - its secret cleanup re-ran the tag runs that exposed this
