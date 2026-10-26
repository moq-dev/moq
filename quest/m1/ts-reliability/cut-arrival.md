# [S] Frames a cut releases are judged by when the loss was seen

## Goal

When a hard-killed source's tracks go unroutable after a silence longer than
the delay, a stitching `export ts --follow` drops the video frames held
through the silence as late and switches to the remaining instance. It never
ends on a missed decode deadline. No flush of held frames carries an arrival
from before a silence into the scheduler.

## Plan

Diagnosis, unconfirmed until reproduced: a SIGKILLed importer is noticed only
after the QUIC idle timeout, so the export sees a silence of that length, then
`Unroutable` makes the consumer emit `Event::Cut`. The cut flushes the frames
held in the track's `DecodeClock` with `Track::release`, which stamps them
`arrived.max(self.latest)`, and `latest` is the last frame's arrival from
before the silence. They pass the jitter buffer as on time, land behind the
schedule's `next`, and `missed()` ends the export. This matches the issue's
table: idle timeouts at or under the 2 s delay never fail, 4 and 6 s do.
[moq-dev/moq#5109](https://github.com/moq-dev/moq/pull/5109) fixed the same
stale stamp when a later frame settles the held ones; a cut settles them with
no frame.

Decided 2026-10-25 (maintainer):

- The internal `Event::Cut` carries the instant the loss was observed, and
  `release` stamps the frames it flushes with `arrived.max(that)`, as the
  settling frame does in #5109. Frames held through a silence drop as late; a
  prompt eviction drops nothing. Rejected: dropping every held frame on a cut
  (it discards on-time frames after a normal eviction), and counting a unit
  queued behind `next` as late in the scheduler (#5109 rejected it: earlier
  units still go out after their DTS).
- Audit every flush path of held frames for the same stale arrival, in both
  the declared and inferred clocks: cut, restart, skip, group end then pending,
  and end of track. End of track keeping its own arrival is deliberate (#5109:
  the stream's end is not a missed deadline); keep it unless the audit finds
  otherwise.
- A unit that reaches the scheduler and still misses its DTS stays a fatal
  error: once stale frames drop in the jitter buffer, a miss there means the
  multiplex rate is too low, and dropping it would hide that.
- Tests are mock-time unit tests in `export_timing_test.rs`, like #5109's: a
  source silent past the delay that ends in a cut, then a case per audited
  path. No nightly CLI rig.

Reproduce first: the cut test must fail on `main` with the issue's error
before the fix. If it does not, the diagnosis is wrong; re-investigate rather
than land the stamp.

Main only: `release` has no `Follower` or `--stitch`, so there is no backport.
Public API: none. Wire: none.

## Closes

- [#6802](https://github.com/moq-dev/moq/issues/6802) - `export ts --stitch --follow`: a hard-killed importer can end the export on a missed decode deadline
