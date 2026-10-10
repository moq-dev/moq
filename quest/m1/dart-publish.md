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

Decided in the 2026-10-10 audit: publication waits for Codecs, the epoch
rename, and named error fields, so Dart takes the binding breaks in one
release. This replaces the 2026-10-05 decision to drop the FFI shape wait.
Trusted-publishing configuration and checks of already-published artifacts
can proceed independently; cutting the tag cannot.

- Confirm trusted publishing is configured for `moq` (repository
  `moq-dev/moq`, tag pattern `moq-dart-v{{version}}`).
- Cut the next `moq-dart-v*` release with the binding breaks and confirm the workflow publishes it.
- Verify the published `moq_ffi` resolves its native asset from a clean
  machine with no monorepo checkout, since that download path is the one CI
  never exercises.

Public API: none. Wire: none.

## Required

- [Codecs](/quest/m1/ffi-shape/codec.md) - settle the audio and video binding API before publishing
- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - include the epoch rename in the same breaking release
- [Named error fields](/quest/m1/ffi-shape/error-fields.md) - include named error payloads in the same breaking release

## Related

- [Dart on iOS](/quest/m1/dart-ios.md) - pub.dev already tags `moq_ffi` for iOS, which nobody has run
