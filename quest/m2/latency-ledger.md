# [M] A viewer's share of playback latency is reportable, not just measurable in a test

## Goal

A running viewer can report where its share of the audio delay went (jitter
buffer, decode, render, and output device), stage by stage, through the
public API in both languages. The numbers a user reads when debugging their
own latency are the numbers the harness grades, because they come from the
same place. Publisher and network stages, and an end-to-end total, wait for
a clock that spans the whole path; none exists today.

## Plan

The audio quality harness (`test/audio-quality/`) landed on ad-hoc debug
probes, which was the right trade to get it running. This quest promotes them.

- Decided 2026-10-08: scope to the viewer's stages. No clock spans
  publisher to viewer, so the harness reports end-to-end as null.
- Take the viewer stages of the schema the harness already defines
  (`jitter_buffer`, `decode`, `render`, `device`) and expose them as fields of
  the hang stats and feedback snapshots (`rs/hang/src/stats.rs`,
  `rs/hang/src/echo.rs`, and their `@moq/hang` mirrors), rather than a
  second readout: an observable value a stats or `.echo` track already
  carries, not a callback. Decided so viewers report latency the same way
  they report stalls.
- Both languages, matching names, per the repo's cross-language rule. Scrutinise
  each exported item: a stage nobody outside can act on stays internal.
- Switch the harness over, deleting the probes it replaces. A ledger with no
  consumer is how this drifts from reality.
- Keep the harness's terms: one duration unit, every timestamp on a named
  clock, and stages as exclusive spans that cannot both claim the same
  milliseconds. Inherit those from the schema rather than restating them,
  so the sum-to-end-to-end identity (with its named `unaccounted`
  remainder) can be added once a spanning clock exists.

## Required

- [Media stats schema](/quest/m1/stats/schema.md) - adds the stats and feedback snapshots this extends

## Related

- [QoS](/quest/m1/qos/README.md) - relay-side health, the same idea from the other end
