# [XS] moq-gst releases publish again

## Goal

`release` carries `main`'s Homebrew formula test, so a `moq-gst-v*` tag's
`Build tarball (aarch64-apple-darwin)` job passes, its `Release` job publishes
the GitHub release, and the next moq-gst release appears under the
repository's releases.

## Plan

The backport, [moq-dev/moq#8109](https://github.com/moq-dev/moq/pull/8109), merged into
`release` at bbecb46a5e1bb08b67bfd399ebc2ed90f27bad1f. It brings
[moq-dev/moq#5841](https://github.com/moq-dev/moq/pull/5841)'s
`.github/homebrew/Formula/moq-gst.rb.tmpl` test to `release`, so the test no longer runs
`gst-inspect-1.0` inside Homebrew's sandbox, where it hangs on a headless Mac. All that
remains is proving it with the next moq-gst tag. Decided 2026-11-06 (maintainer): publish the
next release rather than backfilling 0.4.21 to 0.4.23.

`moq-gst-v*` tags come from release-plz when it publishes the crate (it creates
`<crate>-v<version>` tags, which `sh/gh/release.sh parse-version` reads), so a hand-pushed tag
would collide with it.

Decided 2026-11-07 (maintainer, /quest-unblock): ✅ patch-bump moq-gst to 0.4.24 now
([moq-dev/moq#8128](https://github.com/moq-dev/moq/pull/8128)). Not chosen: waiting for the
next combined release; hand-pushing a tag.

## Waiting

- moq-dev/moq: [moq-dev/moq#8128](https://github.com/moq-dev/moq/pull/8128) merges and release-plz pushes `moq-gst-v0.4.24`. Done when that tag's `Build tarball (aarch64-apple-darwin)` job passes and its GitHub release publishes (`gh release view moq-gst-v0.4.24`); then this PR deletes the quest.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - its secret cleanup re-ran the tag runs that exposed this
