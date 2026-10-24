# [M] Switch moq's bindings to rust2's UniFFI fork

## Goal

moq-ffi depends on the UniFFI fork's renamed crates from crates.io, and every
generator moq uses (Kotlin, Swift, Python, Go, Dart, C++, and C) comes from
[kixelated/rust2](https://github.com/kixelated/rust2) through one pinned flake
input. `rs/uniffi-bindgen`, `nix/uniffi-dart-*`, and the per-generator pins are
deleted, and the three `kixelated/uniffi-*` generator forks are archived.
Generated bindings are unchanged by the switch itself.

## Plan

Decided with the maintainer on 2026-10-13, after a review of how moq uses
UniFFI:

- **Base**: UniFFI. [The BoltFFI spike](/bench/ffi-gen/README.md)
  recommended it. Decided later in #5838 (2026-10-17, maintainer):
  UniFFI-generated moq-ffi serves the browser too, over BoltFFI, so one generator serves every
  language.
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
`kixelated/rust2`, not a new `moq-dev/uniffi`. rust2 groups Rust-to-other-language
tools (bindings, a TypeScript translator, build-speed work) under one setup.
moq's generator code moves with it, including `rs/uniffi-bindgen`. The import
and crates.io publishing are rust2's
[UniFFI import quest](https://github.com/kixelated/rust2/blob/main/quest/m1/uniffi.md),
which publishes `rust2-uniffi`, `rust2-uniffi-core`, `rust2-uniffi-macros`, and
`rust2-uniffi-bindgen`. This quest is the moq side:

- moq-ffi depends on the `rust2-uniffi*` crates directly, with no `[patch]` and
  no git dependency, so crates.io moq-ffi still builds.
- **Generated code stays identical**: Kotlin package names, library symbol
  prefixes, and every wrapper's API do not change. Diff the regenerated
  bindings for every language against `main`'s, and run each wrapper's check
  plus `just test interop --all`.
- **One pin**: rust2 becomes a flake input, and `flake.nix`, the release
  workflows, `rs/moq-ffi/build.sh`, and `sh/` read the generators from it.
  Delete `rs/uniffi-bindgen`, the "five places" comments in `flake.nix`, and the
  `nix/uniffi-dart-*` lockfile and patch. The `sh/{kt,swift,dart,cpp}`
  packaging scripts stay: they package moq.
- Update the quests that name a `kixelated/uniffi-*` fork
  ([Generated C bindings](/quest/m1/c/README.md)) to point at rust2, then
  archive the three forks.

Public API: none; moq-ffi's dependency names change. Wire: none.

## Required

- [rust2 publishes its UniFFI fork](/quest/m2/rust2-uniffi.md) - the crates and generators this switches to

## Related

- [Payload bytes](https://github.com/kixelated/rust2/blob/main/quest/m1/ffi-bytes.md) - the first patch the fork carries
- [Kotlin calls](https://github.com/kixelated/rust2/blob/main/quest/m1/kt-ffi-calls.md) - a Kotlin generator patch the fork carries
- [Foreign poll for hot reads](https://github.com/kixelated/rust2/blob/main/quest/m1/ffi-poll-protocol.md) - a generated poll protocol the fork carries; dropping a future at cancel is optional here, since no moq quest waits on it
