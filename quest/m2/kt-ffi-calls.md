# [M] Kotlin binding calls without JNA overhead

## Goal

A Kotlin call into moq-ffi costs about what the same call costs from Python or
Go, sync and async alike, proven on the
[binding benchmark](/bench/README.md).

## Plan

Measured 2026-10-15: a sync `sequence()` took 11-22 us from Kotlin against
1.3 us from Python and 0.12 us from Go, and the 100 B throughput case kept
about 5.8 cores busy for 2 MB/s.

Decided with the maintainer on 2026-10-15:

- **Profile first** (JFR or async-profiler) and confirm each cause before
  changing anything. The two suspects in the generated `uniffi/moq/moq.kt`:
  - Every call allocates a JNA `Structure` (`UniffiRustCallStatus`, which
    nests a `RustBuffer.ByValue`), and JNA builds and syncs those by
    reflection.
  - The async continuation is a plain JNA `Callback` with no
    `CallbackThreadInitializer`, so each wakeup from a Rust worker attaches
    that thread to the JVM and detaches it again.
- **Fix in [the fork](/quest/m2/uniffi-fork.md)'s Kotlin generator**, not by
  patching generated code. Both costs are one quest: same root, same
  generator, same proof. Prefer a shape that keeps the wrapper's API
  unchanged.
- If [the BoltFFI spike](/quest/m2/boltffi.md) picks BoltFFI, re-plan this
  against its Kotlin backend, or delete it if that backend already avoids JNA.

Public API: none expected. Wire: none.

## Required

- [Hard fork](/quest/m2/uniffi-fork.md) - the generator this patches
