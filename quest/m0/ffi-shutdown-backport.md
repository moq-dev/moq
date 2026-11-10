# [XS] Publish the release line's drain() wrappers

## Goal

The `release` line's hand-written Dart wrapper that gained `drain()` in
[moq-dev/moq#6642](https://github.com/moq-dev/moq/pull/6642), Dart
[`moq`](https://pub.dev/packages/moq) 0.1.1, is on pub.dev.

Check: pub.dev lists the version. It is already set on `release`
(`dart/moq/pubspec.yaml`), so this needs a tag push, not code.

## Plan

Already published (checked 2026-10-31): moq-ffi 0.4.19 carries `drain()` and
qmux 0.5.3 on PyPI, Maven Central, and pub.dev; moq-ffi 0.4.20 followed.
Swift `Moq` 0.5.2 is tagged, and Kotlin `dev.moq:moq` 0.5.3 is on Maven Central
from `gh workflow run release-kt-lib.yml --ref release`
([run](https://github.com/moq-dev/moq/actions/runs/40904381781), after
[moq-dev/moq#7026](https://github.com/moq-dev/moq/pull/7026)'s fix).

Python [`moq-rs`](https://pypi.org/project/moq-rs/) 0.5.2 reached PyPI on
2026-10-31.

Decided 2026-10-30 (maintainer): Dart `moq` 0.1.1 ships in the next combined
patch cut on `release`, which the maintainer cuts.

Dart: pub.dev publishes `moq` only from a `moq-dart-v*` tag, and only
`moq-dart-v0.1.0` exists. Pushing `moq-dart-v0.1.1` at `release`'s head
publishes 0.1.1 and exercises the unproven path in
[A Dart tag publishes unattended](/quest/m3/release/dart-publish.md). Decided
2026-10-30 (maintainer): push it during the combined cut, not before.

Decided 2026-11-04 (maintainer, /quest-unblock): ✅ cut the combined patch
release on `release` soon, once [moq-dev/moq#7596](https://github.com/moq-dev/moq/pull/7596)'s
remaining ports and [moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680)
are ready, and merge `main` into `release` for the next minor line
separately, later. Not chosen: one combined cut that merges `main` first, or
neither this week.

Decided 2026-11-09 (maintainer, /quest-unblock): ✅ an agent pushes
`moq-dart-v0.1.1` at `release`'s head once the combined cut's release-plz
PR, [moq-dev/moq#8378](https://github.com/moq-dev/moq/pull/8378), merges
(rejected: tag before #8378 merges; the maintainer pushes it). Then it checks
that `release-dart.yml` succeeds and pub.dev lists 0.1.1, and deletes this
quest.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: the combined patch cut's release-plz PR,
  [moq-dev/moq#8378](https://github.com/moq-dev/moq/pull/8378), merges into
  `release`. Check with `gh pr view 8378 --repo moq-dev/moq --json state`;
  then push the tag as the Plan's 2026-11-09 decision says.

## Related

- [A Dart tag publishes unattended](/quest/m3/release/dart-publish.md) - proves the `moq-dart-v*` tag path this publish uses
