# [L] Binding durations

## Goal

moq-ffi takes and returns durations as a typed duration instead of about 20
`_us` integer fields, so no wrapper converts microseconds by hand, matching
rs/AGENTS.md's typed-units rule.

## Plan

Planned 2026-10-22 from the API audit
([wrapper config names](/quest/m1/api-audit/wrapper-config.md) fixes the
names in m1). Today Kotlin (`Durations.kt`), Dart (`durations.dart`), Go
(`backoffMicros`), and Python (`timedelta`) each convert. First check that
UniFFI's builtin `Duration` maps through the Go, C++, and Dart generator
forks; if not, it waits on [the generator fork](/quest/m2/uniffi-fork.md).

Public API: moq-ffi, moq-c, every wrapper breaking. Wire: none.

## Related

- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - the same move for media timestamps
