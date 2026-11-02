# [XS] Publish the release line's drain() wrappers

## Goal

The `release` line's hand-written wrappers that gained `drain()` in
[moq-dev/moq#6642](https://github.com/moq-dev/moq/pull/6642) are on their
registries:

- Python [`moq-rs`](https://pypi.org/project/moq-rs/) 0.5.2 on PyPI.
- Dart [`moq`](https://pub.dev/packages/moq) 0.1.1 on pub.dev.

Check: each registry lists the version. Both versions are already set on
`release` (`py/moq-rs/pyproject.toml`, `dart/moq/pubspec.yaml`), so this needs
a publish, not code. Each publish is outward-facing, so the maintainer runs it;
agents do not push tags, dispatch workflows, or bump versions.

## Plan

Already published (checked 2026-10-31): moq-ffi 0.4.19 carries `drain()` and
qmux 0.5.3 on PyPI, Maven Central, and pub.dev; moq-ffi 0.4.20 followed.
Swift `Moq` 0.5.2 is tagged, and Kotlin `dev.moq:moq` 0.5.3 is on Maven Central
from `gh workflow run release-kt-lib.yml --ref release`
([run](https://github.com/moq-dev/moq/actions/runs/40904381781), after
[moq-dev/moq#7026](https://github.com/moq-dev/moq/pull/7026)'s fix).

Decided 2026-10-30 (maintainer): `moq-rs` 0.5.2 and Dart `moq` 0.1.1 ship in
the next combined patch cut on `release`, which the maintainer cuts.

Python: `workflow_run` runs `main`'s `release-py.yml` against the `release`
checkout, which lacked `.github/actions/nix`. The maintainer picked the
backport of that action to `release`, which landed in
[moq-dev/moq#7421](https://github.com/moq-dev/moq/pull/7421). The
`moq-ffi-v0.4.20` tag's chained run
([failed](https://github.com/moq-dev/moq/actions/runs/41038297141)) came before
that merge, and a re-run checks out the same tag commit, so it fails again.
`release-py.yml` has no `workflow_dispatch`. What publishes 0.5.2 now:

- The next `moq-ffi-v*` tag at a `release` commit with the nix action, which
  chains Release Py FFI into Release Py.
- A `release` push touching `py/moq-rs/**`, `.github/workflows/release-py.yml`,
  or `sh/gh/release.sh`: the push gate publishes, since `release`'s moq-ffi
  0.4.20 is tagged and on PyPI.

Dart: pub.dev publishes `moq` only from a `moq-dart-v*` tag, and only
`moq-dart-v0.1.0` exists. Pushing `moq-dart-v0.1.1` at `release`'s head
publishes 0.1.1 and exercises the unproven path in
[A Dart tag publishes unattended](/quest/m2/release/dart-publish.md). Decided
2026-10-30 (maintainer): push it during the combined cut, not before.

Public API: none. Wire: none.

## Related

- [A Dart tag publishes unattended](/quest/m2/release/dart-publish.md) - proves the `moq-dart-v*` tag path this publish uses
