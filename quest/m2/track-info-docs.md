# [XS] The bindings' TrackInfo docs list its real fields

## Goal

The `TrackInfo` alias docs in Swift (`swift/Sources/Moq/Aliases.swift`),
Kotlin (`kt/moq/src/jvmAndAndroidMain/kotlin/dev/moq/Aliases.kt`) and Dart
(`dart/moq/lib/src/aliases.dart`) name only fields `MoqTrackInfo` has. Done
when each alias doc matches the record's fields, and no other binding's
`TrackInfo` doc names a field the record lacks.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729).

The docs say "priority, ordering, latency budget, and timescale" (Kotlin,
Dart) and "priority, group ordering, max age, and timescale" (Swift).
`MoqTrackInfo` (`rs/moq-ffi/src/producer.rs`) has `priority`, `max_age`,
`refresh`, `timescale` and `compress`. Ordering and delay are subscriber-side
(`MoqSubscription`). #7729 adds `max_age_unlimited`, so describe the record
as it stands when this lands.

- Name the fields, or describe the record's purpose without a list that can
  drift. Prefer the one-line purpose plus a pointer to the record, so a new
  field doesn't make the alias wrong again.
- Check the Python, Go, C and C++ wrappers' equivalents the same way.

Public API: docs only. Wire: none.
