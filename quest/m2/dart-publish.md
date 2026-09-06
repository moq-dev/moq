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

Order matters, and the obvious order is wrong. A published `moq_ffi` resolves
its native assets from the GitHub release for its own version, and that release
only exists once the `moq-ffi-v*` tag has run. Publishing by hand first would
ship a package whose hook downloads from a release that does not exist, and the
later tag would then reach `dart pub publish` on an already-published version,
which pub.dev refuses because versions are immutable.

So the assets come first:

- Cut `moq-ffi-v*`. Its build and release jobs attach the per-target libraries
  and their checksums. Expect the publish job to fail: the package name is not
  claimed yet, which is the whole reason for this quest.
- Upload the staged package by hand to claim `moq_ffi`. The `package` job has
  already produced exactly that artifact, so upload it rather than restaging.
- Repeat for `moq` via `moq-dart-v*`, whose only asset is the package itself.
- Configure trusted publishing for each: repository `moq-dev/moq`, tag patterns
  `moq-ffi-v{{version}}` and `moq-dart-v{{version}}`. GitHub OIDC then covers
  every later release and there is no token to store.
- Cut one more patch of each and confirm both workflows publish unattended.

`package.sh` replaces the committed `0.0.0-dev` sentinel with the real version
in `pubspec.yaml`, `hook/build.dart`, and `CHANGELOG.md`, so no version is
hand-edited before tagging. Verify the published `moq_ffi` resolves its native
asset from a clean machine with no monorepo checkout, since that download path
is the one CI never exercises.

Both blockers below are about not publishing a claim we cannot support: the
first release is the one that reaches strangers, and pub.dev packages generally
cannot be unpublished or deleted. A version may be retracted within seven days,
but retraction does not erase it.

## Required

- [Dart binding memory leaks](/quest/m2/dart-leak.md) - publishing a leaking runtime is worse than not publishing
- [Dart on iOS](/quest/m2/dart-ios.md) - the package advertises iOS, which nobody has run
