# [M] A lone TS stream's timestamp step is explained before it is handled

## Goal

The source of a single elementary stream whose PTS jumps alone is found and
reproduced. If it is a real source behavior, such as a muxer or splicer
resetting one stream, the import handles that behavior where it detects it;
if it is corruption nobody can explain, such as a bit flip in a PES header,
the import ends with the cause, as it does today. A data-only program whose
PCR jumps by more than half a wrap is judged the same way.

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

Decided 2026-11-17 (maintainer,
[moq-dev/moq#9010](https://github.com/moq-dev/moq/pull/9010#issuecomment-6959731587)):
✅ a standalone, low-priority m1 quest that does not gate the cut, since no
field report backs it and a lone PID's PTS step needs a corrupt PES header
or a muxer resetting one stream (rejected: gating the cut; deleting it).
✅ This design stands for whenever it is done, reproduced first, and it
challenges [moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898)'s
rule that a TS audio rewind is a restart:
once the step judge rules "not a new source", a packet below the stream's
floor is dropped and counted as that stream's damage, and a forward outlier
raises the floor only when a second packet confirms it (rejected: keeping
the fatal rewind, which lets one bad packet end a live ingest; re-anchoring
as `release` did, which shifts good audio against video).

Superseded 2026-11-18 (maintainer, /quest-plan of the enum-return audit):
✅ investigate the root cause first: "We should not try to support random bit
flips that we don't understand and limp along with broken functionality."
This replaces the 2026-11-17 design above that drops and counts a lone
stream's stray packets as damage (rejected now: tolerating an unexplained
step). [moq-dev/moq#9170](https://github.com/moq-dev/moq/pull/9170) removed
the import's unit damage tolerance; this quest adds back only a step with a
known source, as an explicit case. If no real source turns up, delete this quest and record why.

Tests: a capture or generator reproducing the step's real source, and the
import's behavior on it; for unexplained corruption, the import ends with the
cause.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#7898](https://github.com/moq-dev/moq/pull/7898) - made a lone stream's rewind fatal to the import
- [moq-dev/moq#5967](https://github.com/moq-dev/moq/pull/5967) - PCR unwrap that can go negative on a data-only program

## Related

- [moq-dev/moq#9035](https://github.com/moq-dev/moq/pull/9035) - plans `quest/m2/ts-passthrough-splice.md`, which handles a whole-source PCR step on the passthrough lane; this quest is the lone-stream step on the import
