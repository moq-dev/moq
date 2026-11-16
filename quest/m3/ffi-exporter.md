# [L] Switch moq's bindings to rust2's exporter

## Goal

moq's bindings (Kotlin, Swift, Python, Go, Dart, C++, and C) come from
[rust2's exporter](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export/README.md)
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
[moq-dev/rust2#41](https://github.com/moq-dev/rust2/pull/41), that the exporter
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
dependency, so crates.io moq-ffi still builds (see the
[condition quest](/quest/m3/rust2-exporter.md)).

- **Generated code stays identical**: Kotlin package names, library symbol
  prefixes, and every wrapper's API do not change, except Dart's
  `MoqRoute.instanceId()` below. Diff the regenerated bindings for every
  language against `main`'s, and run each wrapper's check plus
  `just test interop --all`. The C names and that Dart rename below are the
  only expected differences.
- **C names** (maintainer, 2026-10-24 and 2026-10-25): rust2's C generator
  names in GLib style
  ([moq-dev/rust2#66](https://github.com/moq-dev/rust2/pull/66)).
  Types are CamelCase with the namespace prefix (`MoqError`, `MoqStringList`,
  enum tags `<Enum>Tag`, async callbacks `<Function>Callback`), functions and
  enum constants keep their names, and any remaining clash fails generation
  with both items named. `c/moq` (its probe, `shutdown.c`, and docs) moves to
  the new type names. moq-ffi's record `MoqAudioEncoderFinished` returns to
  `MoqAudioEncoderFinish`, its name before
  [moq-dev/moq#6052](https://github.com/moq-dev/moq/pull/6052) worked around
  the old clash with `moq_audio_encoder_finish`; every wrapper's
  `EncoderFinish` alias follows, so wrapper APIs do not change.
- **Dart `MoqRoute.instanceId()`** (maintainer, 2026-11-07, from
  [moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)'s API audit):
  the switch renames it to the exporter's name for moq-net's
  `Route::instance()`, a Dart API change the PR's Impact section records for
  the release cut's `MIGRATE.md`. Open: rust2's C++ generator declares a
  private `uint64_t instance` field in every object class
  (`uniffi/cpp/bindgen/src/bindings/cpp/templates/obj.hpp`), which is why
  moq-ffi names the method `instance_id` (`rs/moq-ffi/src/origin.rs`). Before
  the rename, either rename rust2's private field (recommended, since rust2
  is ours and the clash would hit any `instance` method), or keep the
  generated `instance_id` and add the Dart name as a wrapper alias.
- rust2's C backend still refuses `Duration`, which moq-ffi's audio configs
  use (moq's current generator accepts it), so the fork must accept it before
  the switch can generate C.
- **One pin**: rust2 becomes a flake input, and `flake.nix`, the release
  workflows, `rs/moq-ffi/build.sh`, and `sh/` read the generators from it.
  Delete `rs/uniffi-bindgen`, the "five places" comments in `flake.nix`, and the
  `nix/uniffi-dart-*` lockfile and patch. The `sh/{kt,swift,dart,cpp}`
  packaging scripts stay: they package moq.
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
  [moq-dev/rust2#364](https://github.com/moq-dev/rust2/pull/364) mirrors them
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
land before the cut instead, in
[the selector quest](/quest/m2/ffi-open-selectors.md) and the bindings'
group rename; the switch keeps those shapes. The audit's other binding findings were not picked: the `Moq`
facade Kotlin and Dart start from stays, and `MoqError`'s variants stay as
they are.

Public API: breaking in every binding. moq-ffi's dependency names change,
generated C type names become CamelCase, the generated record
`MoqAudioEncoderFinished` becomes `MoqAudioEncoderFinish` in every language,
Dart's `MoqRoute.instanceId()` takes the exporter's name, and any other
generated name the wrappers re-export may change, each listed in the PR.
Wire: none.

## Required

- [rust2's exporter is ready for moq](/quest/m3/rust2-exporter.md) - the exporter generates every language moq ships and moq can pin it

## Related

- [Payload bytes](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-bytes.md) - the first patch the fork carries
- [Kotlin calls](https://github.com/moq-dev/rust2/blob/main/quest/m1/kt-ffi-calls.md) - a Kotlin generator patch the fork carries
- [Foreign poll for hot reads](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-poll-protocol.md) - a generated poll protocol the fork carries; dropping a future at cancel is optional here, since no moq quest waits on it
