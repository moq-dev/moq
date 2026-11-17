# [M] A TS import survives one stream's timestamp step

## Goal

A single elementary stream whose PTS jumps alone (a bit error or splicer
glitch on one audio PID) damages only that stream: the import drops and
counts the stray packets and keeps running. A data-only program whose PCR
jumps by more than half a wrap counts the reading as unplaced instead of
failing the import.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source; reproduce
first. In `rs/moq-mux/src/container/ts/import.rs`, `forward` (called for
AAC, Opus, and legacy audio) returns `TimestampRewind`, which becomes
`Fault::Fatal`. `step.rs` rules that a stream stepping alone "is not a new
source and is left to the import", yet the import then fails the ingest: one
AAC packet 50 minutes ahead raises the floor, and the next normal packet ends
everything. `release`'s `Reanchor` shifted a stepped track instead. The
data-only case: `place_pcr` with no media clock lets `PtsUnwrap` go
negative, and `u64::try_from` fails the import with `TimeOverflow`, where a
program with media drops the same packet as unplaced.

Recommended (challenges [moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898)'s
rule that a TS audio rewind is a restart, for the maintainer's review): ✅
once the step judge rules "not a new source", a packet below the stream's
floor is dropped and counted as that stream's damage, and a forward outlier
raises the floor only when a second packet confirms it (rejected: keeping
the fatal rewind, which lets one bad packet end a live ingest; re-anchoring
as `release` did, which shifts good audio against video).

Tests: PCR, H.264, and AAC on one timeline with one AAC packet 30 s ahead,
then normal packets: `decode` keeps succeeding and counts one damaged
packet; and a data-only program with a PCR jump past half a wrap.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898) - made a lone stream's rewind fatal to the import
- [moq-dev/moq#5967](https://github.com/moq-dev/moq/pull/5967) - PCR unwrap that can go negative on a data-only program
