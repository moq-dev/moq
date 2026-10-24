# [L] Binding durations

## Goal

moq-ffi takes and returns durations as a typed duration instead of about 20
`_us` integer fields, so no wrapper converts microseconds by hand, matching
rs/AGENTS.md's typed-units rule.

## Plan

Planned 2026-10-22 from the API audit
([wrapper config names](/quest/m1/api-audit/wrapper-config.md) fixes the
names in m1). Today Kotlin (`Durations.kt`), Dart (`durations.dart`), Go
(`backoffMicros`), and Python (`timedelta`) each convert.

Moved to m1 on 2026-10-23 (maintainer): the wrapper config rename already
breaks these options this release, so binding users migrate them once.
Checked 2026-10-23 against the pinned generators: UniFFI's builtin
`Duration` maps to `time.Duration` in Go, Dart's `Duration`, and
`std::chrono` nanoseconds in C++, and Python, Swift, and Kotlin are built
into UniFFI. Only the generated C refuses it, which the C prerequisite below
fixes.

Public API: moq-ffi, moq-c, every wrapper breaking. Wire: none.

## Required

- [Durations in generated C](/quest/m1/c/duration.md) - the C backend renders UniFFI's `Duration` instead of refusing it

## Related

- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - the same move for media timestamps
