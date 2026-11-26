# [L] Switch moq's bindings to rust2's exporter

## Goal

moq's bindings (Kotlin, Swift, Python, Go, Dart, C++, and C) come from
[rust2's exporter](https://github.com/kixel-ai/rust2/blob/main/ffi/README.md)
(`rust2-ffi`), which exports moq's crates' `pub` API as a C ABI with no
attributes. moq-ffi shrinks to the remainder that is genuinely new API.
`rs/uniffi-bindgen`, `nix/uniffi-dart-*`, the per-generator pins, and the
UniFFI dependency are deleted, and the three `kixelated/uniffi-*` generator
forks are archived. The switch is a breaking release of every binding
(decided 2026-11-16): the hand-written parts of each wrapper keep their API
except where a decision below says so, and the generated names the wrappers
re-export may change.

## Plan

Decided 2026-11-02 (maintainer, /quest-unblock): moq skips rust2's UniFFI fork and switches straight to the
exporter. rust2 decided on 2026-10-25, from
[kixel-ai/rust2#41](https://github.com/kixel-ai/rust2/pull/41), that the exporter
replaces the fork, which stays in maintenance; publishing `rust2-uniffi` to
crates.io would commit permanent names to code already slated for
retirement, and switching moq to it would be churn on the way to the
exporter. Until the switch, moq keeps building from its pinned
`kixelated/uniffi-*` forks, checked each release cycle by
[the vendor sync](/quest/m3/vendor-sync/README.md).
Not chosen: publish the fork and switch to it first; decide later.

The bullets below carried over from the fork plan; each still applies to the
exporter's generators and needs re-checking against its design. The fork-era
choices (UniFFI as the base, a hard fork in rust2 published as the
`rust2-uniffi*` crates) no longer apply: the fork is not published, and
moq-ffi builds on the exporter's pinned revision with no `[patch]` and no git
dependency, so crates.io moq-ffi still builds: rust2-ffi is a build tool from
rust2's flake, and the crate it generates depends only on crates.io packages
and the facade.

The exporter is ready (checked 2026-11-22): rust2's exporter epic finished with
[kixel-ai/rust2#955](https://github.com/kixel-ai/rust2/pull/955), which added
the flake package moq pins, `github:kixel-ai/rust2/<rev>#ffi`, and
[kixel-ai/rust2#956](https://github.com/kixel-ai/rust2/pull/956), its last
child. rust2's README lists every generator moq ships. The package wraps
rust2-ffi with rust2's std sysroot, so moq's stable cargo runs it.

- **API diff across every language**: Kotlin package names, library symbol
  prefixes, and the hand-written wrapper surfaces keep their API except where
  the decisions below say so; generated names the wrappers re-export may change
  (see the 2026-11-16 decision below). Diff the regenerated bindings for every
  language against `main`'s, and run each wrapper's check plus
  `just test interop --all`.
- **C names** (maintainer, 2026-10-24 and 2026-10-25): rust2's C generator
  names in GLib style
  ([kixel-ai/rust2#66](https://github.com/kixel-ai/rust2/pull/66)).
  Types are CamelCase with the namespace prefix (`MoqError`, `MoqStringList`,
  enum tags `<Enum>Tag`, async callbacks `<Function>Callback`), functions and
  enum constants keep their names, and any remaining clash fails generation
  with both items named. `c/moq` (its probe, `shutdown.c`, and docs) moves to
  the new type names. moq-ffi's record `MoqAudioEncoderFinished` returns to
  `MoqAudioEncoderFinish`, its name before
  [moq-dev/moq#6052](https://github.com/moq-dev/moq/pull/6052) worked around
  the old clash with `moq_audio_encoder_finish`; every wrapper's
  `EncoderFinish` alias follows.
- **Generated C async failure contract** (folded from the retired C async
  failure contract quest, decided 2026-10-28 and 2026-11-02;
  reference [kixel-ai/rust2#169](https://github.com/kixel-ai/rust2/pull/169)):
  every generated async call returns a task (never NULL), and its callback runs
  exactly once on the dispatcher (`moq_set_dispatcher`), carrying `bool cancelled`
  right after `user_data` (when set, `error` is NULL and `value` is empty; a
  Rust panic aborts). `c/moq/test/probe.c`'s callbacks take `cancelled` and
  release `user_data`.
- **Route identity name**: [Rename Instance to Identity](/quest/m2/identity-rename-and-cap.md)
  renames `Route::instance()` to `Route::identity()` in `m2`, which also avoids
  the old C++ private `uint64_t instance` clash that forced `MoqRoute.instanceId()`.
- Verify rust2's C backend accepts `Duration` (used by moq-ffi's audio configs).
- **One pin**: rust2 becomes a flake input, its `ffi` package the exporter, and `flake.nix`, the release
  workflows, `rs/moq-ffi/build.sh`, and `sh/` read the generators from it.
  Delete `rs/uniffi-bindgen`, the "five places" comments in `flake.nix`, and the
  `nix/uniffi-dart-*` lockfile and patch. The `sh/{kt,swift,dart,cpp}`
  packaging scripts stay, except the Swift FFI ones the release template below
  replaces.
- **Python and Swift FFI releases** (maintainer, 2026-11-17, from
  [kixel-ai/rust2#852](https://github.com/kixel-ai/rust2/pull/852), which added
  rust2's [release template](https://github.com/kixel-ai/rust2/blob/main/ffi/release/README.md)):
  moq-ffi's wheels and XCFramework come from rust2's shared `bindings.yml`
  builder, published by the `release.yml` that
  `package.py init --manifest-path rs/moq-ffi/Cargo.toml --swift-mirror moq-dev/moq-swift-ffi`
  writes at the rust2 commit moq pins, plus `features: openh264`. It publishes
  on the `moq-ffi-v*` tags moq already cuts. Register it as `moq-ffi`'s PyPI
  trusted publisher in place of `release-py-ffi.yml`, then delete
  `release-py-ffi.yml`, `release-swift-ffi.yml`,
  `sh/swift/{package,verify,publish}-ffi.sh`, and the maturin section of
  `pyproject.toml`. `release-swift-lib.yml`, `release-py.yml`, and
  `sh/swift/{package,verify,publish}.sh` keep packaging the hand-written
  wrappers, against the new FFI package.
  - ✅ Floors rise to Python 3.11 (the stable ABI's first with the buffer
    protocol), macOS 13 and Swift 6.2, with iOS 16 unchanged, and moq-ffi
    ships no sdist, since a source build needs the nightly exporter
    (rejected: keep Python 3.10; keep macOS 12.3 by gating `Swift.Duration`
    APIs; a pre-exported sdist).
  - ✅ Delete the Swift FFI packaging scripts the template replaces (rejected:
    keep them), so one path packages the FFI.
  - ✅ Accept the exporter's names: the Python import stays `moq_ffi`, the
    Swift module becomes `MoqFfi` and the asset `MoqFfiFFI.xcframework.zip`
    (today both `MoqFFI`), listed in `MIGRATE.md` (rejected: keep `MoqFFI`,
    which needs a rename the exporter has no way to express).
  - ✅ A new `swift-mirror` environment holds moq-bot's App credentials,
    deployable only from `moq-ffi-v*` tags, so moq's `release.yml` stays
    exactly what `package.py init` writes and a rust2 pin bump is a re-init
    (rejected: hand-editing moq's copy to the existing `bot` environment;
    renaming the template's environment in rust2 to moq's name).
- **Dart FFI release** (from
  [kixel-ai/rust2#915](https://github.com/kixel-ai/rust2/pull/915), kept by the
  maintainer 2026-11-22 for planning here): `moq_ffi` becomes the pub package
  the export writes in `dart/`, with rust2's shared build hook, released by the
  `pub` job that the same `package.py init` writes. That job publishes through
  pub.dev's automated publishing on the `moq-ffi-v*` tags, after the GitHub
  release hosts the libraries the package's `hook/release.json` names by size
  and SHA-256; without that file, as in a checkout, the hook builds the crate
  with Cargo. The package name is the library's prefix, so it stays `moq_ffi`.
  It lands in the switch, not after it, since the re-init brings the `pub`
  job, which would publish beside `release-dart-ffi.yml`, and deleting UniFFI
  removes what generates today's package.
  - Point `moq_ffi`'s automated publishing on pub.dev at a new `pub`
    environment (tag pattern `moq-ffi-v{{version}}`), deployable only from
    `moq-ffi-v*` tags, as `swift-mirror` above; `release-dart-ffi.yml` uses
    `pub.dev` today. An
    agent can make the change by driving the maintainer's signed-in Chrome.
  - Delete `dart/moq_ffi` (its hook, `pubspec.yaml`, packaging, and tests that
    the export replaces), `dart/uniffi.toml`, `release-dart-ffi.yml`, its
    packaging in `sh/dart/package.sh`, and its leg of the shared native build
    (`release-shared-ffi.yml`, `sh/gh/ffi.test.ts`). The `moq` wrapper
    (`dart/moq`, `release-dart.yml`) stays, depending on the published
    `moq_ffi` and, in a checkout, on the export's `dart/` by path.
  - Consumers lose what rust2 rejected in #915: the hook's `user_defines`
    override of the library (a checkout builds from source instead), the
    `x86_64-apple-ios` simulator library (Intel Macs; the source build still
    maps it), and the `.sha256` release assets (`release.json` pins them).
    Floors are Flutter's defaults, Android 24, iOS 13.0, and macOS 10.15, and
    the Dart SDK stays at 3.10. List the changes in `moq_ffi`'s `MIGRATE.md`.
  - Decided 2026-11-25 (maintainer, /quest-run): ✅ `moq_ffi` accepts rust2's
    pub package as it is, losing these three, as for Python and Swift, so a
    pin bump stays a re-init and every binding ships one way (rejected:
    adding `x86_64-apple-ios` to rust2's builder; keeping moq's own hook
    beside the export's).
  - [Dart on iOS](/quest/m4/dart-ios.md)'s verdict then applies to rust2's
    package: dropping the iOS targets, if it comes to that, is a change in
    rust2's builder rather than in `release-dart-ffi.yml`.
- Zero-default durations (Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), from
  [moq-dev/moq#7697](https://github.com/moq-dev/moq/pull/7697)): the
  exporter gives generated duration fields a zero default, so a bare C++
  `moq::Subscription{}` or generated C `moq_subscription_default()` means a
  max delay of 0, as every hand-written binding's does. Until then those
  two leave both bounds unset, and their docs say to start from
  `subscription_defaults()` (accepted docs-only on #7697). It lands in
  this quest.
- Negative durations (2026-10-25, maintainer): the Dart negative-duration
  patch moq carries goes upstream into the fork's Dart generator, and the Go
  generator stops panicking on a negative `time.Duration` (the Go wrapper
  guards it today; delete the guard in the PR that pins a generator with the
  fix, with a test passing a negative duration through the Go binding).
- Python running-loop preference and Dart buffer ownership (2026-10-31, from
  [moq-dev/moq#7461](https://github.com/moq-dev/moq/pull/7461)):
  [kixelated/uniffi-rs#1](https://github.com/kixelated/uniffi-rs/pull/1)
  carries Python's running-loop preference (`f903c36c`), and
  [kixelated/uniffi-dart#7](https://github.com/kixelated/uniffi-dart/pull/7)
  carries Dart's nested input-scratch cleanup (`71dc9f8b`) and disposal status
  reservation (`a2751c4c`).
  [kixel-ai/rust2#364](https://github.com/kixel-ai/rust2/pull/364) mirrors them
  into the maintenance fork. The exporter's generators must retain these
  behaviors too, rechecking each against their design before the switch.
- Update the quests that name a `kixelated/uniffi-*` fork
  ([Generated C bindings](/quest/m3/c/README.md)) to point at rust2, then
  archive the three forks.
- Cancelled Go results (2026-10-31, from
  [moq-dev/moq#7397](https://github.com/moq-dev/moq/pull/7397)):
  uniffi-bindgen-go `v0.10.0-kixelated.2` destroys a result whose context was
  cancelled as the call completed, and completes a future cancelled while
  pending as `context.Canceled`; the exporter's Go bindings must do both. The
  Go generator's fix that destroys a cancelled async call's result lives on
  `kixelated/uniffi-bindgen-go`'s `cancelled-result` branch and tag
  `v0.10.0-kixelated.2+v0.32.0`, not on the fork's `main`, like the earlier
  fork tags. The exporter's Go generator must destroy that result too: port
  the fork's `destroy` fixture tests to it before archiving the fork. It
  should also destroy the error value of a cancelled call that completed
  with an error, which the fork still leaks (moq-ffi's errors hold no
  handles today).

Deferred 2026-11-15 (maintainer, /quest-complete): the `Waiting` decision on
the generated binding renames stays open, kept out of that run's m0 to m2
scope, since the binding API changes anyway in the rust2 migration expected
around 2026-12-15.

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit), settling
that `Waiting` item: ✅ accept two breaks. 0.22 ships the bindings on
UniFFI, and the switch is a second breaking binding release (rejected:
announcing the binding packages as a preview that freezes at the switch;
holding the binding packages off the shared minor until the switch; counting
only the hand-written wrappers as public API, the old option B).

Option B's premise does not hold, which is why it was not offered again: the
generated layer is public API in most languages, counted 2026-11-16.

- Kotlin: 187 `typealias` lines under `kt/moq/src/jvmAndAndroidMain` onto
  `uniffi.moq.*` classes, handles included (104 in `Aliases.kt`), and a test
  requires one for every generated record and handle
  (`SmokeTest.kt:224`).
- Dart: 157 `typedef` lines under `dart/moq/lib`.
- Swift wraps handles but has 102 `typealias` lines under
  `swift/Sources/Moq` (46 in `Aliases.swift`), among them `MoqError`,
  `ProtocolKind`, `AnnounceEvent`, and `ConnectionPhase`.
- C++ and the generated C are generated entirely.

So "wrapper APIs do not change" in the bullets above, carried over from the
abandoned fork plan, is a property to measure, not assume: the switch's PR diffs each
language's public API (`sh/api-diff`) and its Impact section lists every
rename for the packages' `MIGRATE.md`. Reshapes that outlive the generator
land before the cut instead: the open-set rule in `rs/moq-ffi/AGENTS.md`
(selector objects, `Unknown` variants, and a null `max_age` for no limit) and
the group rename; the switch keeps those shapes. The audit's other binding findings were not picked: the `Moq`
facade Kotlin and Dart start from stays, and `MoqError`'s variants stay as
they are.

Public API: breaking in every binding. moq-ffi's dependency names change,
generated C type names become CamelCase, every generated C async callback gains
`bool cancelled`, the generated record
`MoqAudioEncoderFinished` becomes `MoqAudioEncoderFinish` in every language,
the FFI Swift module
becomes `MoqFfi`, moq-ffi's floors rise to Python 3.11, macOS 13, and Swift
6.2 with no sdist, `moq_ffi` loses its `user_defines` override and its
x86_64 iOS simulator library, and any other generated name the wrappers
re-export may change, each listed in the PR.
Wire: none.

## Required

- [Release cut](/quest/m3/release/cut.md) - 0.22.0 ships bindings on UniFFI; the exporter switch is the second breaking binding release after the cut (decided 2026-11-16)
