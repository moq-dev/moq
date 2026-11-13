# [L] @moq/net addresses groups by start

## Goal

`@moq/net` matches Rust: groups are addressed by start timestamp in its API,
model, lite-07 wire, and legacy mapping, with the same names.

## Plan

Mirror [the model](/quest/m2/time-addressing/model.md) and
[the lite-07 wire](/quest/m2/time-addressing/lite07.md), including strictly
increasing starts, the three group ends, reach from a group's own end, and
`Relative(n)` counting groups. The gap machinery that assumes `sequence + 1`
(`delivered.ts`, `ledger.ts`, `track.ts`, `resume.ts`) follows
previous-start links instead. Starts are `bigint`, as group IDs already are,
so they are carried past 2^53 (decided 2026-11-03, maintainer).
`Time.Timestamp.value` stays a `number` for now (2026-11-08, the JS time
types PR, to keep frames allocation-free); revisit it here if a start must
compare exactly with a timestamp past 2^53 ticks.

The spike found no sparse-ID breakage in direct `@moq/net` sessions over
lite-06 and lite-07; its relay fills follow Rust's once JavaScript received
copies fill. JS ranges
([moq-dev/moq#7696](https://github.com/moq-dev/moq/pull/7696)), which the
2026-10-27 decision required first, merged.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ this waits for
Lite-07 FETCH is single-group only ([moq-dev/moq#8616](https://github.com/moq-dev/moq/pull/8616)),
which moves JS multi-group fills (`js/net/src/lite/subscriber.ts`
`#runFills`) onto the subscription this rekeys.

Public API: breaking, mirroring Rust's. Wire: as the lite-07 quest.

## Required

- [Lite-07 by start](/quest/m2/time-addressing/lite07.md) - the wire this speaks
