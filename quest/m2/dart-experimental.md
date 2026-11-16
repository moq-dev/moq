# [S] Dart ships as experimental, with its bindings generated at build time

## Goal

The Dart packages say what they are: experimental. `dart/moq/README.md`,
`doc/lib/dart`, and the language table in `doc/lib/index.md` label Dart
experimental, and the `moq` wrapper on pub.dev keeps its own 0.x version
instead of joining the shared 0.22 minor, so the 0.22 announcement does not
promise Dart what it promises Swift, Kotlin, Python, Go, and C++.

`dart/moq_ffi/lib/src/moq.dart` and `uniffi_runtime.dart` are no longer in
git. They are generated when the package is checked, tested, and published,
and the published `moq_ffi` archive still contains them.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ Dart
is experimental, off the shared minor, and its generated layer is built
instead of checked in (rejected: shipping it beside the other languages as
planned; publishing only `moq_ffi` and dropping the wrapper). The other
languages each have evidence Dart lacks, and a 0.22 version would claim it.

Facts found by the audit:

- No user: neither moq.pro nor Kixel has a quest that names Dart. C++ has
  `cpp/obs`; Dart has nothing in the tree.
- Never loaded on its main platform:
  [Dart on iOS](/quest/m4/dart-ios.md) says nobody has loaded it. There is
  no example application (`dart/README.md` says so), and no Dart client in
  `test/interop/clients` (c, cpp, go, js, js-native, python, rust). A tag
  has never published the wrapper unattended
  ([Dart publication](/quest/m3/release/dart-publish.md)).
- It is kept in step mechanically, so it is maintained, not proven.
- `dart/moq_ffi/lib/src/moq.dart` is 25,932 generated lines in git, and 163
  first-parent commits since the last release rewrote it
  (`git log --oneline d0b086e71b..HEAD -- dart/moq_ffi/lib/src/moq.dart`).
  No other language commits its generated layer. `sh/dart/check.sh:88-93`
  diffs the checked-in file against a fresh generation and fails when it is
  stale, which is the only reason it is committed.
- The wrapper is already versioned on its own (0.1.1 on pub.dev), but
  `sh/version.py` lists it in `WRAPPERS` (`:52`), so the cut's
  `just version` would set it to the shared minor.

For the PR:

- Add `"dart": ["moq"]` to `version.json`'s exemptions. `moq_ffi` keeps
  tracking `rs/moq-ffi`'s version, since it is that crate's artifact and
  its native asset download is keyed by it. Say in `RELEASE.md` why Dart is
  exempt and what ends the exemption: an interop client or the iOS proof.
- Untrack both generated files and ignore them. `just dart check`,
  `dart test`, and the Native Assets hook generate them first
  (`sh/dart/generate.sh` already writes them); drop the staleness diff.
  `release-dart-ffi.yml` generates before `dart pub publish`, and the PR
  shows with `dart pub publish --dry-run` that the archive holds both
  files. A consumer installing from pub.dev must not need the generator.
- Label: one sentence at the top of `dart/moq/README.md`,
  `dart/moq_ffi/README.md`, and `doc/lib/dart/index.md` saying the API may
  change in any release and what is unproven, and "experimental" in the
  Dart row of `doc/lib/index.md`.
- Dart still follows AGENTS.md's Cross-Package Sync: a binding feature
  reaches it with the other languages. Experimental describes the promise,
  not the upkeep.
- Update [Dart publication](/quest/m3/release/dart-publish.md)'s floor
  check if the exemption changes what `just version` writes into
  `dart/moq/pubspec.yaml`.

Public API: none changes; the Dart wrapper's version stays below the shared
minor. Wire: none.

## Related

- [Dart on iOS](/quest/m4/dart-ios.md) - the proof that would end the experimental label
- [A Dart tag publishes unattended](/quest/m3/release/dart-publish.md) - the publish this changes the inputs of
