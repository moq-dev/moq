# [XS] TS stopped log grades only audio and video

## Goal

The TS import and export stats log ("elementary stream stopped delivering
access units") fires only for audio and video PIDs. Sparse data PIDs such as
SCTE-35 and ID3 are still counted but never logged as stopped.

## Plan

`ts::Log::sample` (`rs/moq-mux/src/container/ts/stats.rs`), added for SRT
ingest in [#4506](https://github.com/moq-dev/moq/pull/4506) and used for export
since [#4577](https://github.com/moq-dev/moq/pull/4577), logs any PID whose
access-unit count did not move in a 1 s interval. That assumes a continuous
stream; SCTE-35 sends a section now and then, so it logs nearly every quiet
second after each cue.

Decision (2026-10-01): ✅ grade only audio/video. Rejected: learning each
PID's usual gap (more logic) and leaving the noise.

`Log::sample` only sees `Stats`, and `StreamStats` carries no stream type; the
`track` suffix can't tell (`.ts` is any verbatim data, `""` is MPEG-1/2 video).
Carry the classification import already resolved into `StreamStats` for active,
retired, and section rows, not the raw PMT `stream_type`: `0x86` is DTS or,
with a CUEI descriptor, SCTE-35, and import routes the latter to sections.
Test a sparse CUEI-marked `0x86` PID beside a stalled video PID.

Public API: an additive field on the `#[non_exhaustive]` `StreamStats`. Wire:
none.

## Related

- [TS stats module](/quest/m1/ts-stats-module.md) - renames these types
- [TS health stats](/quest/m2/ts-health-stats.md) - publishes the same per-PID liveness
