# [XL] Rust groups are addressed by their start

## Goal

In `rs/moq-net`, a group's identity is its start timestamp: the API and the
model carry no group sequence numbers, a reader seeks or starts from a time,
and no producer remembers group numbers. Lite-01..06 and moq-transport peers
keep working through a group ID equal to the start in the track's timescale,
and lite-07-wip carries that same value until
[the lite-07 wire](/quest/m1/time-addressing/lite07.md) replaces it.

## Plan

Decided 2026-10-25 (maintainer). Why: group numbers need timelines just to
map to and from time, and reusing a group number poisons caches, so resuming
a producer would need per-track memory of its last number.

- **A group's address is its first frame's PTS**, known when the group opens.
  Frames inside a group stay in decode order and their PTS may go backwards
  (B-frames).
- **Starts strictly increase.** An equal start is refused loudly like a
  rewind; `Neighbors::admit` (`model/track.rs`, about line 502) allows ties today.
- **Three ways a group ends**: `finish(end)` with an exclusive end timestamp
  (the group is whole), `close()` (incomplete, but every frame written
  arrives), and `abort(err)` (reset). A relay closes an expired or partial
  group instead of resetting it.
- **Reach** (decided 2026-10-26, maintainer): a finished group's own end
  bounds it, else its successor's start, so the last group of a paused or
  finished track ages out without a successor (`TrackState::reach`).
- **Every track is timed.** Untimed goes away: the producer stamps an untimed
  write with wall-clock time in microseconds, which stays under 2^53 (the
  published JS decode limit, below) until the year 2255.
- **Gaps.** The model keeps each group's previous start, so a cache or reader
  sees a hole without a round trip. The first group of an epoch has none, and
  the gap before it is unknown. Restarts keep minting a new epoch with fresh
  PTS; replicas in one epoch agree by PTS without coordinating numbers.
- **Legacy egress.** The group ID on lite-01..06 and moq-transport is the
  start in the track's timescale, so every relay agrees without a table.
  Published `@moq/net` decodes a group ID with `u53` and refuses one past
  2^53 (`js/net/src/lite/group.ts`), so a track whose starts pass that, such
  as nanosecond epoch time, fails those readers.
- **Legacy ingest.** A timed source keeps its timestamps. An untimed source
  (moq-transport without TIMESCALE, lite-01..04) uses the group ID as the
  timestamp on a synthetic scale, so relays agree and legacy egress
  round-trips the exact ID; max age means nothing on those tracks. A timed
  moq-transport group that starts at or before its predecessor ends the
  track with an error.
- **Ranges.** `Absolute(group)` ends and sequence-range drops become times.
  `Relative(n)` stays but counts groups instead of subtracting from an ID.
- **Name reuse** landed in
  [moq-dev/moq#6780](https://github.com/moq-dev/moq/pull/6780); the cut
  removes its sequence half, leaving a re-created track to keep its PTS
  moving forward.
- **What it waits on** (decided 2026-10-27, maintainer): only the
  subscribe-ranges children that put sequence ranges into the model and the
  wire, [Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md),
  [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md), and
  [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md),
  not the whole epic. [Drop set](/quest/m1/drop-set.md) lands first too: its
  runs keyed by group sequence convert mechanically to starts.
- **Old relays** (decided 2026-10-26, maintainer): a released relay fills a
  hole over lite-05/06 with one FETCH per integer ID, so sparse IDs cost it
  one FETCH per tick in the hole. Accepted: "This was a lingering problem.
  The entire point of (multiple) ranges is to avoid this issue."

Every current role of the sequence needs a replacement or a reason to drop
it:

- The cache key, the duplicate check, the hand-out-once record
  (`delivered.rs`), and slot stamps.
- Successor, reach, expiry, the arrival ledger, tail accounting, fills and
  misses, and SUBSCRIBE_DROP accounting. Successor and reach already scan the
  cache by range, so they tolerate sparse keys; `delivered.rs` runs, fills,
  misses, and `subscription/window.rs` treat `sequence + 1` as the next group
  and must follow previous-start links instead.
- Resume across route flips (`resume.rs`), mid-group `Position` and
  `frame_start`, priority order within a subscription, `Demand::wanted`, and
  refresh.
- The datagram namespace, which shares the group space.
- The legacy codecs: lite GROUP, SUBSCRIBE and SUBSCRIBE_UPDATE bounds,
  SUBSCRIBE_START and its largest, SUBSCRIBE_END, SUBSCRIBE_DROP, FETCH, and
  every IETF Location, Filter, FETCH, Largest, and NEW_GROUP_REQUEST.

### Spike evidence

The spike published groups at their start ticks (2 s GOPs at 1 kHz and at
90 kHz) over lite-03, lite-06, lite-07-wip, moq-transport-14, and
moq-transport-18, direct and through a relay, against a contiguous baseline
(prototype tests in
[moq-dev/moq@ebdfd55](https://github.com/moq-dev/moq/commit/ebdfd55fc606db8f2c990b937392b81291ab4715)).

- Unaffected on `main`: live delivery through `recv_group` and `ordered()`,
  history with `Range::oldest` and absolute ranges, max-age expiry, a fetch
  of a missing ID (prompt `NotFound`), and hole fills, which take a constant
  handful of FETCHes at any spacing since
  [moq-dev/moq#6724](https://github.com/moq-dev/moq/pull/6724) and
  [moq-dev/moq#6715](https://github.com/moq-dev/moq/pull/6715). Before them
  a relay opened one FETCH per ID (540,000 for one 90 kHz hole), which is
  what released relays still do. `@moq/net` matched on lite-06 and lite-07.
- Broken: `Relative(1)..` on lite-07, direct or relayed, resolves by ID
  arithmetic, delivering `[6000]` and then parking instead of
  `[4000, 6000]`.

Update `doc/concept`, the lite draft's legacy mapping, and the
Rust half of every consumer the API change breaks to compile;
[consumers](/quest/m1/time-addressing/consumers.md) and
[bindings](/quest/m1/time-addressing/bindings.md) own the behavior changes.
Test the legacy mapping both ways on every wire, with the
resume model (`rs/moq-net/src/model/resume_model.rs`,
`js/net/src/resume.model.test.ts`) as the oracle for resume.
The PR's Impact section says how to migrate.

Public API: breaking. `group::Info`, `create_group`, `append_group`,
`Position`, `Range` ends, `fetch_group`, `Subscriber::floor`, datagrams, and
group consumers address by start timestamp, and untimed tracks go away.
Wire: no format change; legacy and lite-07-wip group IDs become start ticks.

## Required

- [Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md) - the one-range reads this rework moves onto times
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the range ends and drops this rework moves onto times
- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - the lite cursor state lives in the model before the cut rekeys it
- [Drop set](/quest/m1/drop-set.md) - its dropped and expired runs convert mechanically from sequences to starts
