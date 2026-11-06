# [XS] moq-gst releases publish again

## Goal

`release` carries `main`'s Homebrew formula test, so a `moq-gst-v*` tag's
`Build tarball (aarch64-apple-darwin)` job passes, its `Release` job publishes
the GitHub release, and the next moq-gst release appears under the
repository's releases.

## Plan

Found 2026-11-06 (/quest-unblock) while re-running
[moq-dev/moq#7936](https://github.com/moq-dev/moq/pull/7936)'s failed tag runs.
Every moq-gst tag since 0.4.20 failed the same way:
[0.4.21](https://github.com/moq-dev/moq/actions/runs/41573749359),
[0.4.22](https://github.com/moq-dev/moq/actions/runs/41711099214), and
[0.4.23](https://github.com/moq-dev/moq/actions/runs/42129723974) (also on
its 2026-11-06 re-run). The `Test Homebrew formula against tarball` step
(`just gh gst-formula "$VERSION" "$TARGET" dist`, which runs
`sh/gh/gst-formula.py`) runs `brew test moq-dev/ci-gst/moq-gst`, whose
`test do` calls `gst-inspect-1.0`. That hangs at startup inside Homebrew's
test sandbox on a headless Mac, so the step hits Homebrew's 5-minute timeout
and `Release` is skipped. Those three GitHub releases never published; their
crates did.

`main` already fixed this in
[moq-dev/moq#5841](https://github.com/moq-dev/moq/pull/5841): its
`.github/homebrew/Formula/moq-gst.rb.tmpl` test checks that the plugin exists
and links `libgstreamer-1.0` instead of loading it, while the workflow still
loads the plugin into GStreamer outside the sandbox. `release` got the
formula step in the backport `be7f785b` but kept the pre-#5841 test. The fix
is a backport of that template change to `release`; the rest of the
`release`/`main` difference in `moq-gst.yml` (the Nix action and artifact
pins) is unrelated. The next moq-gst tag then proves it.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ plan it in m0, and publish
the next release rather than backfilling 0.4.21 to 0.4.23. Not chosen:
backfilling each missed version; m3 with the post-cut release work.

Public API: none. Wire: none.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - its secret cleanup re-ran the tag runs that exposed this
