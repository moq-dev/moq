# [XS] A Dart tag publishes unattended

## Goal

A `moq-dart-v*` tag publishes `moq` to pub.dev with no manual step, as
`moq-ffi-v*` tags already publish `moq_ffi`.

## Plan

Both packages are on pub.dev (checked 2026-10-20). `moq_ffi` publishes from
every `moq-ffi-v*` tag (0.4.3 through 0.4.17, the latest 2026-10-17), so its
tag-driven publish works. `moq` has only 0.1.0 (2026-09-25), the one
`moq-dart-v*` tag so far, so whether its tag publishes unattended through
trusted publishing is still unproven.

Trusted-publishing configuration and checks of already-published artifacts
can proceed independently; cutting the tag cannot.

- Confirm trusted publishing is configured for `moq` (repository
  `moq-dev/moq`, tag pattern `moq-dart-v{{version}}`).
- Raise `dart/moq/pubspec.yaml`'s `moq_ffi` floor to the release the wrapper now needs (the media namespace and codec types), since `^0.4.3` resolves bindings without them.
- Cut the next `moq-dart-v*` release with the binding breaks and confirm the workflow publishes it.
- Verify the published `moq_ffi` resolves its native asset from a clean
  machine with no monorepo checkout, since that download path is the one CI
  never exercises.

Public API: none. Wire: none.

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - prepared code reaches release before artifact publication and installed proof


## Related

- [Dart on iOS](/quest/m2/dart-ios.md) - pub.dev already tags `moq_ffi` for iOS, which nobody has run
