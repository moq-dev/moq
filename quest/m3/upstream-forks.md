# [S] Offer the uniffi generator fixes upstream

## Goal

Every general fix our uniffi generator forks carry has been offered to its
upstream, or recorded as declined or MoQ-specific, so each fork shrinks
toward a pin on an upstream tag. Very low priority: the forks work, and the
first step is someone else's review. Every external post, issue, or PR needs
the maintainer's approval at the time it is made.

One outcome per candidate:

- **uniffi-bindgen-cpp** (`kixelated/uniffi-bindgen-cpp`, forked from
  LiveKit's `uniffi-0.31-async`, PR #1;
  [#4100](https://github.com/moq-dev/moq/pull/4100)): the two leak fixes (a
  ready Rust future freed without `rust_future_complete`, and a dropped
  foreign future that never completed its oneshot), the missing
  `#include <string>` MSVC needs, the uniffi 0.32 port, and
  `error_style = "expected"`. The bug fixes are the easy offer; the 0.32 port
  and the expected style depend on LiveKit taking async at all.
- **uniffi-dart** (`kixelated/uniffi-dart`;
  [#4072](https://github.com/moq-dev/moq/pull/4072)): the
  `nix/uniffi-dart-record-error.patch` and the RustBuffer release fixes. Fix
  the latent `lowerForeignBytes` leak first (borrowed `&[u8]` arguments
  allocate a `ForeignBytes` nothing frees; the free belongs after the call,
  not inside the lowering), and audit callback interfaces for the same
  RustBuffer leak, so the upstream offer is complete. `moq_ffi` uses neither
  today.
- **uniffi-rs Python typing** of data-carrying enum variants
  ([#4049](https://github.com/moq-dev/moq/pull/4049)): the generated type
  makes `moq.VideoEncoderKind.AUTO()` fail pyright, so
  `doc/lib/py/index.md` carries a `pyright: ignore`. Fix it at the source
  and drop the ignore once a release carries it.

## Plan

- Decided in #4100: fork tags keep upstream's `v<generator>+v<uniffi>`
  scheme with a `-kixelated.N` pre-release, e.g.
  `v0.11.0-kixelated.1+v0.32.2`, matching the Dart fork. The suffix sorts
  after the base release and before any upstream patch release, and never
  collides with an upstream tag. The Go fork (`v0.9.0+v0.32.0`) moves to it
  at its next bump.
- Offer each fix with the regression test it landed with. When upstream
  merges one, move the pin in `flake.nix` (and the places its comment lists)
  and delete the carried patch.
- Record each outcome (merged, declined with the reason, or not offered
  because it is MoQ-specific) in the PR that finishes this quest.

Public API: none. Wire: none.

## Related

- [Upstream the fork](/quest/m1/quic/upstream.md) - the same practice for the noq fork
- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the line that forked the C++ generator
