# [M] Switch moq's bindings to rust2's UniFFI fork

## Goal

moq-ffi depends on the UniFFI fork's renamed crates from crates.io, and every
generator moq uses (Kotlin, Swift, Python, Go, Dart, C++, and C) comes from
[moq-dev/rust2](https://github.com/moq-dev/rust2) through one pinned flake
input. `rs/uniffi-bindgen`, `nix/uniffi-dart-*`, and the per-generator pins are
deleted, and the three `kixelated/uniffi-*` generator forks are archived.
The browser's TypeScript, generated through ubrn's `wasm2` flavour, still
generates against the fork. Generated bindings are unchanged by the switch
itself, except C's type names and the one record rename the C name rule
undoes.

## Plan

Decided with the maintainer on 2026-10-13, after a review of how moq uses
UniFFI:

- **Base**: UniFFI. [The BoltFFI spike](https://github.com/moq-dev/rust2/blob/main/bench/ffi/README.md#boltffi-comparison)
  recommended it. Decided later in #5838 (2026-10-17, maintainer):
  UniFFI-generated moq-ffi serves the browser too, through ubrn's `wasm2`
  flavour (chosen over BoltFFI and hand-written moq-wasm), so one binding
  surface serves every language.
- **Why fork**: every UniFFI minor release breaks external generators (0.31
  changed `module_path`, 0.32 changed the metadata encoding without bumping
  the contract version, and `main` removes the old library-mode API). No
  external generator has released for 0.32, so moq carries three forks, each
  pinned in about five places. The core also blocks work moq needs: byte
  copies, zero-copy returns, and namespaced external types
  (mozilla/uniffi-rs#3026, auto-closed by our
  [#5498](https://github.com/moq-dev/moq/pull/5498) and still unfixed
  upstream).
- **Free to diverge**: the fork may change metadata, checksums, and the FFI
  contract when a moq quest needs it. A diverged contract must never mix
  silently with upstream UniFFI.

Decided with the maintainer on 2026-10-23: the hard fork lives in
`moq-dev/rust2`, not a new `moq-dev/uniffi`. rust2 groups Rust-to-other-language
tools (bindings, a TypeScript translator, build-speed work) under one setup.
moq's generator code moves with it, including `rs/uniffi-bindgen`. The import
and crates.io publishing are rust2's
[UniFFI import quest](https://github.com/moq-dev/rust2/blob/main/quest/m1/uniffi.md),
which publishes `rust2-uniffi`, `rust2-uniffi-core`, `rust2-uniffi-macros`, and
`rust2-uniffi-bindgen`. This quest is the moq side:

- moq-ffi depends on the `rust2-uniffi*` crates directly, with no `[patch]` and
  no git dependency, so crates.io moq-ffi still builds.
- **Generated code stays identical**: Kotlin package names, library symbol
  prefixes, and every wrapper's API do not change. Diff the regenerated
  bindings for every language against `main`'s, and run each wrapper's check
  plus `just test interop --all`. The C names below are the only expected
  differences.
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
- rust2's C backend still refuses `Duration`, which moq-ffi's audio configs
  use (moq's current generator accepts it), so the fork must accept it before
  the switch can generate C.
- **One pin**: rust2 becomes a flake input, and `flake.nix`, the release
  workflows, `rs/moq-ffi/build.sh`, and `sh/` read the generators from it.
  Delete `rs/uniffi-bindgen`, the "five places" comments in `flake.nix`, and the
  `nix/uniffi-dart-*` lockfile and patch. The `sh/{kt,swift,dart,cpp}`
  packaging scripts stay: they package moq.
- **Browser TypeScript**: ubrn's `wasm2` generator is upstream and reads
  UniFFI metadata, which the fork may change. Either show upstream ubrn still
  generates from the renamed crates, or carry ubrn's TypeScript generator in
  rust2 with the others, as [ubrn 0.32](/quest/m2/rs2ts/ubrn-032.md) already
  plans if ubrn has not moved. Include the TypeScript in the regenerated
  bindings diff.
- Negative durations (2026-10-25, maintainer): the Dart negative-duration
  patch moq carries goes upstream into the fork's Dart generator, and the Go
  generator stops panicking on a negative `time.Duration` (the Go wrapper
  guards it today; delete the guard in the PR that pins a generator with the
  fix, with a test passing a negative duration through the Go binding).
- Update the quests that name a `kixelated/uniffi-*` fork
  ([Generated C bindings](/quest/m2/c/README.md)) to point at rust2, then
  archive the three forks.

Public API: moq-ffi's dependency names change, generated C type names become
CamelCase, and the generated record `MoqAudioEncoderFinished` becomes
`MoqAudioEncoderFinish` in every language; wrapper APIs do not change. Wire:
none.

## Required

- [rust2 publishes its UniFFI fork](/quest/m2/rust2-uniffi.md) - the crates and generators this switches to

## Related

- [UniFFI generator forks are current](/quest/m1/vendor-sync/uniffi.md) - keeps the forks this replaces synced with upstream until the switch; its fixes must reach rust2's fork
- [ubrn generates UniFFI 0.32 TypeScript](/quest/m2/rs2ts/ubrn-032.md) - the browser generator this switch must keep working
- [Payload bytes](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-bytes.md) - the first patch the fork carries
- [Kotlin calls](https://github.com/moq-dev/rust2/blob/main/quest/m1/kt-ffi-calls.md) - a Kotlin generator patch the fork carries
- [Foreign poll for hot reads](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-poll-protocol.md) - a generated poll protocol the fork carries; dropping a future at cancel is optional here, since no moq quest waits on it
