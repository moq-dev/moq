# [L] Namespaced external types in the UniFFI fork

## Goal

Two crates linked into one library can each export a type with the same
name, and a type in one can take the other's: `media.GroupConsumer(group:
GroupConsumer)` generates and runs in Kotlin, Swift, Python, Go, Dart, C++,
and C from [moq-dev/uniffi](/quest/m2/uniffi-fork.md).

## Plan

Decided with the maintainer on 2026-10-13: fix
[mozilla/uniffi-rs#3026](https://github.com/mozilla/uniffi-rs/issues/3026)
in the fork instead of waiting for upstream.

- `TypeUniverse` keys types by `(module_path, name)` instead of bare name,
  and each generator qualifies cross-crate references through the other
  crate's module. The #5460 and #5473 probes found Python and Go already
  reference external types that way. Kotlin and Dart shadow them, Swift
  needs per-crate modules plus an import, C++ sorts by bare name, and
  generated C refuses external types.
- Reuse the two-crate spike harness from #5460 as the regression: both
  layers export `GroupConsumer` and `Frame`, and every generator compiles
  and runs the consumer.
- Re-running the per-layer crate split decision is a follow-up once this
  lands. [FFI shape](/quest/m1/ffi-shape/README.md) points here as its
  revisit condition.

Public API: none in moq until the crate split is re-decided. Wire: none.

## Required

- [Hard fork](/quest/m2/uniffi-fork.md) - the repository the fix lands in
