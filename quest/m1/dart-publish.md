# [XS] A Dart tag publishes unattended

## Goal

A `moq-dart-v*` tag publishes `moq` to pub.dev with no manual step, as
`moq-ffi-v*` tags already publish `moq_ffi`.

## Plan

Found in the 2026-10-05 audit: both packages are on pub.dev. `moq_ffi` has
eight versions (0.4.3 to 0.4.10, published 2026-09-25 to 2026-10-03), so its
tag-driven publish works. `moq` has only 0.1.0 (2026-09-25), the one
`moq-dart-v*` tag so far, so whether its tag publishes unattended through
trusted publishing is still unproven.

Decided in the 2026-10-05 audit: the quest shrinks to that check. The FFI
shape wait is gone, since Dart is no longer unpublished; FFI shape carries a
Dart upgrade note instead.

- Confirm trusted publishing is configured for `moq` (repository
  `moq-dev/moq`, tag pattern `moq-dart-v{{version}}`).
- Cut the next `moq-dart-v*` patch and confirm the workflow publishes it.
- Verify the published `moq_ffi` resolves its native asset from a clean
  machine with no monorepo checkout, since that download path is the one CI
  never exercises.

Public API: none. Wire: none.

## Related

- [Dart on iOS](/quest/m1/dart-ios.md) - pub.dev already tags `moq_ffi` for iOS, which nobody has run
