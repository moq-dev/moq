# [S] Dart packages on pub.dev

## Goal

`moq` and `moq_ffi` are installable from pub.dev, and later `moq-ffi-v*` and
`moq-dart-v*` tags publish without hand-holding.

## Plan

The release workflows landed with
[#3215](https://github.com/moq-dev/moq/pull/3215) and pass their dry runs, but
nothing is published: pub.dev requires the first version of a new package to be
uploaded by hand before automated publishing can be configured. So this is
one-time setup rather than code.

- Upload the first `moq_ffi` and `moq` versions manually to claim both names.
- Configure trusted publishing for each: repository `moq-dev/moq`, tag patterns
  `moq-ffi-v{{version}}` and `moq-dart-v{{version}}`. GitHub OIDC then covers
  every later release and there is no token to store.
- Cut the tags and confirm `release-dart-ffi.yml` attaches the native assets
  and publishes, and that `release-dart.yml` publishes the wrapper.

`package.sh` replaces the committed `0.0.0-dev` sentinel with the real version
in `pubspec.yaml`, `hook/build.dart`, and `CHANGELOG.md`, so no version is
hand-edited before tagging. Verify the published `moq_ffi` resolves its native
asset from a clean machine with no monorepo checkout, since that download path
is the one CI never exercises.

Both blockers below are about not publishing a claim we cannot support: the
first release is the one that reaches strangers, and pub.dev versions cannot be
unpublished after 24 hours.

## Required

- [Dart binding memory leaks](/quest/m0/dart-leak.md) - publishing a leaking runtime is worse than not publishing
- [Dart on iOS](/quest/m3/dart-ios.md) - the package advertises iOS, which nobody has run
