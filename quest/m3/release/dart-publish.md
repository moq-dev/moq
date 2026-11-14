# [XS] A Dart tag publishes unattended

## Goal

A `moq-dart-v*` tag publishes `moq` to pub.dev with no manual step, as
`moq-ffi-v*` tags already publish `moq_ffi`.

## Plan

Both packages are on pub.dev (checked 2026-10-25). `moq_ffi` publishes from
every `moq-ffi-v*` tag (0.4.3 through 0.4.18, the latest 2026-10-23), so its
tag-driven publish works. `moq` has 0.1.0 (2026-09-25) and 0.1.1
(2026-11-11, published by a rerun of its tag's workflow, below), so whether a
tag publishes unattended through trusted publishing, with no rerun, is still
unproven.

Trusted-publishing configuration and checks of already-published artifacts
can proceed independently; cutting the tag cannot.

- Trusted publishing for `moq` is fixed: its repository field read
  `moq/moq-dev` (owner and name swapped), so pub.dev refused the
  `moq-dart-v0.1.1` tag's token. On 2026-11-11 the maintainer set it to
  `moq-dev/moq` (tag pattern `moq-dart-v{{version}}`, environment `pub.dev`,
  push events), and a rerun of that tag's
  [release-dart run](https://github.com/moq-dev/moq/actions/runs/43107848443)
  published 0.1.1. The next tag shows whether it publishes with no rerun.
- The cut's `just version` raises `dart/moq/pubspec.yaml`'s `moq_ffi` floor from `^0.4.3`, which resolves bindings without the media namespace and codec types (`FFI_REQUIREMENTS` in `sh/version.py`). Check the published `pubspec.yaml` carries the cut's floor.
- Cut the next `moq-dart-v*` release with the binding breaks and confirm the workflow publishes it.
- Verify the published `moq_ffi` resolves its native asset from a clean
  machine with no monorepo checkout, since that download path is the one CI
  never exercises.

Public API: none. Wire: none.

## Required

- [Breaking release cut](/quest/m3/release/cut.md) - prepared code reaches release before artifact publication and installed proof


## Related

- [Dart on iOS](/quest/m4/dart-ios.md) - pub.dev already tags `moq_ffi` for iOS, which nobody has run
