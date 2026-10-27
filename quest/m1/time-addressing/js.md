# [L] @moq/net addresses groups by start

## Goal

`@moq/net` matches Rust: groups are addressed by start timestamp in its API,
model, lite-07 wire, and legacy mapping, with the same names.

## Plan

Mirror [the model](/quest/m1/time-addressing/model.md) and
[the lite-07 wire](/quest/m1/time-addressing/lite07.md), including strictly
increasing starts, the three group ends, reach from a group's own end, and
`Relative(n)` counting groups. The gap machinery that assumes `sequence + 1`
(`delivered.ts`, `ledger.ts`, `track.ts`, `resume.ts`) follows
previous-start links instead. Starts stay JavaScript numbers, so a decoded
start past 2^53 is refused as today's group IDs are.

The spike found no sparse-ID breakage in direct `@moq/net` sessions over
lite-06 and lite-07; its relay fills follow Rust's once
[JS ranges](/quest/m1/subscribe-ranges/js.md) land. Decided 2026-10-27
(maintainer): this requires JS ranges, so the ranges it rekeys exist first.

Public API: breaking, mirroring Rust's. Wire: as the lite-07 quest.

## Required

- [Lite-07 by start](/quest/m1/time-addressing/lite07.md) - the wire this speaks
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the JS range model and relay fills this moves onto starts
