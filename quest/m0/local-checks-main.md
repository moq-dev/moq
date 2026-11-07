# [S] Local checks pass on unchanged main

## Goal

`just ci` and `just check` pass in a clean checkout of `main` on the
maintainer's desktop, so an agent's local gate fails only for its own change.

## Plan

Added 2026-11-06 (maintainer, follow-up planning in `/quest-complete`). Several
`/quest-iterate` agents this session saw the same local failures on unchanged
`main`:

- `test_interop_consumers_survive_collection` in
  `test/interop/staging.test.py`. Pullfrog's
  [8f2668b5ab](https://github.com/moq-dev/moq/commit/8f2668b5ab64a5c5a046f3f685e6581d4dbcaec9),
  pushed to [moq-dev/moq#8001](https://github.com/moq-dev/moq/pull/8001) and
  left out of it, fixes two causes: `bare-fin` in `test/justfile` builds
  moq-net without `async`, which its async-gated `read_frame` needs, and the
  staging check seeds only the examples named in `$EXAMPLES`, missing
  `interop-socket-js`. Port it as is.
- The direnv fixture failures and the supervised JavaScript executable lookup
  ([moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981)'s agent
  reproduced both on unchanged `main`).
- The CARLA launcher environment assertion
  ([moq-dev/moq#7986](https://github.com/moq-dev/moq/pull/7986)'s agent).
- `sh/go/publish-wrapper.test.sh` under a global `tag.gpgsign = true`: its `git tag`
  makes a signed annotated tag, opens the editor for a message, and exits non-zero, so
  "cut v0.7.0" and "tagged the pushed HEAD" fail and `just check`'s `test gh` with them
  ([moq-dev/moq#8124](https://github.com/moq-dev/moq/pull/8124) reproduced it on
  unchanged `main`). Run the test's git with an isolated config, such as
  `GIT_CONFIG_GLOBAL=/dev/null`, so no host setting reaches it. Added 2026-11-07
  (maintainer, follow-up planning in moq-dev/rust2's `/quest-complete`).

Reproduce each on a clean `main` first and keep the output; fix each at its
cause. A failure that only reproduces under load joins
[More tests under load](/quest/m0/test-flakes-2/README.md) instead.

Public API: none. Wire: none.
