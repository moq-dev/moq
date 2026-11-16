# [L] @moq/net addresses groups by start

## Goal

`@moq/net` matches Rust: groups are addressed by start timestamp in its API,
model, lite-07 wire, and legacy mapping, with the same names.

## Plan

Mirror [the model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md) and
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

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ a
frame's position in its group is its `index`, renamed in this PR (rejected:
a later rename; keeping `sequence`). Once groups are keyed by `start`,
`sequence` would survive in JS meaning only a frame index, while Rust says
`group::Consumer::index()`, `skip_to(index)`, and `start_at(index)`
(`rs/moq-net/src/model/group.rs:4203`, `:4243`, `:1757`), and JS's own
`Track.Position.frame` and `js/net/INTEGERS.md` already say index. Renamed:
`Frame.Consumer.sequence` (`js/net/src/frame.ts:251`), and
`Group.Consumer.readFrameSequence` and `tryReadFrameSequence`
(`js/net/src/group.ts:1406`, `:1363`), whose result field is `sequence`.
Callers outside `js/net/src`: `js/e2ee/src/group.ts` (2) and two
benches. ✅ In the same PR, `Time.keyOf` becomes a getter on `Timestamp`
(rejected: keeping the free function). It is an `export let` assigned from
inside the class (`js/net/src/time.ts:53`, `:128`) with one caller,
`js/json/src/time.ts:11` and `:23`; the split between `value` and the exact
key stays as decided 2026-11-08.

Public API: breaking, mirroring Rust's. Wire: as the lite-07 quest.

## Required

- [Lite-07 by start](/quest/m2/time-addressing/lite07.md) - the wire this speaks
