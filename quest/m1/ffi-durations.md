# [L] Binding durations

## Goal

moq-ffi takes and returns durations as a typed duration instead of about 20
`_us` integer fields, so no wrapper converts microseconds by hand, matching
rs/AGENTS.md's typed-units rule.

## Plan

Planned 2026-10-22 from the API audit. Today Kotlin (`Durations.kt`), Dart
(`durations.dart`), Go (`backoffMicros`), and Python (`timedelta`) each
convert.

Moved to m1 on 2026-10-23 (maintainer): the wrapper config rename already
breaks these options this release, so binding users migrate them once.
Checked 2026-10-23 against the pinned generators: UniFFI's builtin
`Duration` maps to `time.Duration` in Go, Dart's `Duration`, and
`std::chrono` nanoseconds in C++, and Python, Swift, and Kotlin are built
into UniFFI. The generated C renders it as `moq_duration`, `int64_t`
nanoseconds like C++, since `uniffi-bindgen-cpp` `v0.11.0-kixelated.7`.
C and C++ abort on a duration past `INT64_MAX` nanoseconds (about 292
years), so moq-ffi must not hand out a saturated `Duration::MAX` where it
clamps to `u64::MAX` microseconds today (`auth.rs`, `session.rs`).

Public API: moq-ffi, moq-c, every wrapper breaking. Wire: none.

## Related

- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - the same move for media timestamps
