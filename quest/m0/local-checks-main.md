# [XS] Local checks pass on unchanged main

## Goal

`just ci` and `just check` pass in a clean checkout of `main` on the
maintainer's desktop, so an agent's local gate fails only for its own change.

## Plan

Added 2026-11-06 (maintainer, follow-up planning in `/quest-complete`).
[moq-dev/moq#8104](https://github.com/moq-dev/moq/pull/8104) showed the
earlier failures (staging fixture and `bare-fin`, direnv fixtures, the
supervised JavaScript executable lookup, the CARLA launcher assertion) no
longer reproduce on `main`, so one remains:

- `sh/go/publish-wrapper.test.sh` under a global `tag.gpgsign = true`: its `git tag`
  makes a signed annotated tag, opens the editor for a message, and exits non-zero, so
  "cut v0.7.0" and "tagged the pushed HEAD" fail and `just check`'s `test gh` with them
  ([moq-dev/moq#8124](https://github.com/moq-dev/moq/pull/8124) reproduced it on
  unchanged `main`). Run the test's git with an isolated config, such as
  `GIT_CONFIG_GLOBAL=/dev/null`, so no host setting reaches it. Added 2026-11-07
  (maintainer, follow-up planning in moq-dev/rust2's `/quest-complete`).

Reproduce it on a clean `main` first and keep the output; fix it at its cause.

Public API: none. Wire: none.
