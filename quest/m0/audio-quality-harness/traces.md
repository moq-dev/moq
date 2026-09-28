# [M] Real arrival traces are recorded, checked in, and graded

## Goal

A trimmed fixture of real audio arrival traces lives in the repository, and the
audio quality harness grades a replay of it next to its synthetic profiles. The
[jitter target's watch quest](/quest/m0/audio-jitter-target/watch.md) replays
the same fixture through both rings in `replay.test.ts`.

## Plan

The #3477 traces are gone, so record fresh ones: a viewer on the public relay
playing `bbb.hang` (the AAC flush-span shape the issue measured), a local relay
as the shallow control, and a browser microphone published through the public
relay. Stamp each audio frame's arrival where the container consumer receives
it, with its media timestamp and group, on the viewer's monotonic clock (see
`test/audio-quality/clients/js/src/schema.ts`).

The browser lane reads only public signals, and no public signal carries a
per-frame arrival, so decide where the recorder lives: a debug hook in
`@moq/watch` that the harness page enables, or a native subscriber (`moq`
CLI) that records the same path's arrivals. The reporter's fork (`fperex/moq`,
branch `debug-findings-solution`) recorded from a debug sampler in the player
and replayed through `js/watch/src/audio/replay.ts` on a simulated clock;
that replay module is the likely shape for the grading side, and landing it is
a player change this quest owns.

Replay rows are deterministic, so their budgets are exactly what was measured,
with no headroom.

## Related

- [Latency ledger](/quest/m2/latency-ledger.md) - would promote an arrival stamp into public API
