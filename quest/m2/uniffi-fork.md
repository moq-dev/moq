# [L] Hard-fork the binding generator

## Goal

One moq-dev repository holds UniFFI's core and every generator moq uses
(Kotlin, Swift, Python, Go, Dart, C++, and C), builds them from one lockfile,
and releases them under one tag. moq-ffi depends on the fork's crates from
crates.io under moq names, and moq names that release in one pin file. The
three `kixelated/uniffi-*` generator forks are archived. Generated bindings
are unchanged by the move itself.

## Plan

Decided with the maintainer on 2026-10-13, after a review of how moq uses
UniFFI:

- **Base**: [the BoltFFI spike](/quest/m2/boltffi.md) decides whether the
  fork starts from UniFFI or BoltFFI. The plan below assumes UniFFI. If
  BoltFFI is chosen, re-plan this quest against it (the repo name, the
  crate names, porting the Go, Dart, and C++/C generators to BoltFFI) along
  with the quests that name UniFFI internals:
  [Payload bytes](/quest/m2/ffi-bytes.md),
  [Poll-native async](/quest/m2/ffi-poll.md),
  [Zero-copy byte returns](/quest/m2/ffi-bytes-return.md),
  [Kotlin calls](/quest/m2/kt-ffi-calls.md), and
  [Namespaced external types](/quest/m2/uniffi-namespaces.md).

- **Why**: every UniFFI minor release breaks external generators (0.31
  changed `module_path`, 0.32 changed the metadata encoding without bumping
  the contract version, and `main` removes the old library-mode API). No
  external generator has released for 0.32, so moq already carries three
  forks, each pinned in about five places. The core also blocks work moq
  needs: byte copies, zero-copy returns, and namespaced external types
  (mozilla/uniffi-rs#3026).
- **Hard fork, fresh repo**: `moq-dev/uniffi` is a new repository, not a
  GitHub fork and not submodules. Import uniffi-rs at the 0.32.2 release and
  the three generators at their pinned tags
  (`kixelated/uniffi-bindgen-go`, `kixelated/uniffi-dart`,
  `kixelated/uniffi-bindgen-cpp` with its C backend) into one Cargo
  workspace with a committed lockfile. Record each import's upstream source
  and revision, and keep each component's license (uniffi-rs is MPL-2.0).
- **Free to diverge**: the fork may change metadata, checksums, and the FFI
  contract when a moq quest needs it. Upstream changes are cherry-picked when
  useful, not tracked. Offering a patch upstream is optional and needs
  maintainer approval each time.
- **Publishing**: the fork publishes renamed crates to crates.io (for example
  `moq-uniffi`, `moq-uniffi-core`, `moq-uniffi-macros`,
  `moq-uniffi-bindgen`), and moq-ffi depends on them directly with no
  `[patch]`. crates.io moq-ffi must build without git dependencies, and a
  diverged contract must never mix silently with upstream UniFFI. Give the
  contract its own namespace, so a mismatch fails loudly at load time.
- **Generated code stays identical at the move**: Kotlin package names,
  library symbol prefixes, and every wrapper's API do not change in this
  quest. Diff the regenerated bindings for every language against `main`'s,
  and run each wrapper's check plus `just test interop --all`.
- **One pin**: replace the per-generator pins with one file that
  `flake.nix`, the release workflows, `rs/moq-ffi/build.sh`, and `sh/`
  read. Delete the "five places" comments in `flake.nix` and the
  `nix/uniffi-dart-*` lockfile and patch. `rs/uniffi-bindgen` builds the
  Kotlin/Swift/Python CLI from the fork.
- **The fork's CI** builds every generator and runs its fixture suite,
  including the unlocked `cargo install` resolve that bit the 0.32 port.
- Update the quests that name a `kixelated/uniffi-*` fork
  ([Generated C++ shape](/quest/m1/cpp-generated-shape.md),
  [Generated C bindings](/quest/m1/c/README.md)) to point at moq-dev/uniffi.

Public API: none at the move; moq-ffi's dependency names change. Wire: none.

## Required

- [BoltFFI or UniFFI](/quest/m2/boltffi.md) - picks the base this fork starts from

## Related

- [Payload bytes](/quest/m2/ffi-bytes.md) - the first patch the fork carries
- [Kotlin calls](/quest/m2/kt-ffi-calls.md) - a Kotlin generator patch the fork carries
- [Namespaced external types](/quest/m2/uniffi-namespaces.md) - fixes mozilla/uniffi-rs#3026 in the fork
