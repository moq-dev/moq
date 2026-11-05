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

- **A group's address is its key.** A group our producers write is keyed by
  its first frame's PTS, known when the group opens. A group ingested from a
  legacy wire is keyed by the wire's group ID, verbatim (decided 2026-11-03,
  below). Frames inside a group stay in decode order and their PTS may go
  backwards (B-frames).
- **Starts strictly increase.** An equal start is refused loudly like a
  rewind; `Neighbors::admit` (`model/track.rs`, about line 502) allows ties today.
- **Three ways a group ends**: `finish(end)` with an exclusive end timestamp
  (the group is whole), `close()` (incomplete, but every frame written
  arrives), and `abort(err)` (reset). A relay closes an expired or partial
  group instead of resetting it.
- **Reach** (decided 2026-10-26, maintainer): a finished group's own end
  bounds it, else its successor's start, so the last group of a paused or
  finished track ages out without a successor (`TrackState::reach`).
- **Every write takes a timestamp, and nothing is ever wall-clock stamped**
  (decided 2026-11-03, below). A track without a timescale is scale-less: its
  keys and frame timestamps are bare numbers in no known unit.
- **Gaps.** The model keeps each group's previous start, so a cache or reader
  sees a hole without a round trip. The first group of an epoch has none, and
  the gap before it is unknown. Restarts keep minting a new epoch with fresh
  PTS; replicas in one epoch agree by PTS without coordinating numbers.
- **Legacy egress.** The group ID on lite-01..06 and moq-transport is the
  start in the track's timescale, so every relay agrees without a table.
  Published `@moq/net` decodes a group ID with `u53` and refuses one past
  2^53 (`js/net/src/lite/group.ts`), so a track whose starts pass that, such
  as nanosecond epoch time, fails those readers.
- **Legacy ingest.** Every legacy wire's group ID is taken verbatim as the
  key, timed or not, so relays agree and legacy egress round-trips the exact
  ID (decided 2026-11-03, below). A group whose ID is at or before its
  predecessor's ends the track with an error.
- **Ranges.** `Absolute(group)` ends and sequence-range drops become times.
  `Relative(n)` stays but counts groups instead of subtracting from an ID.
- **Name reuse** landed in
  [moq-dev/moq#6780](https://github.com/moq-dev/moq/pull/6780); the cut
  removes its sequence half, leaving a re-created track to keep its PTS
  moving forward.
- **What it waits on** (decided 2026-10-27, maintainer; Fetch as a range
  finished in [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)): only the
  subscribe-ranges children that put sequence ranges into the model and the
  wire, Lite-07 ranges ([moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872)) and
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
- Not datagrams: they become their own track kind with their own sequence ([Datagram tracks](/quest/m1/datagram-tracks/README.md)).
- The legacy codecs: lite GROUP, SUBSCRIBE and SUBSCRIBE_UPDATE bounds,
  SUBSCRIBE_START and its largest, SUBSCRIBE_END, SUBSCRIBE_DROP, FETCH, and
  every IETF Location, Filter, FETCH, Largest, and NEW_GROUP_REQUEST.

### Timestamps are required and legacy IDs pass through

Decided 2026-11-03 (maintainer, the `/quest-import` grill of
[moq-dev/moq#7573](https://github.com/moq-dev/moq/issues/7573)). Why: a
wall-clock or `now()` stamp on a write lands in a different epoch than the
broadcast's media. That is what gives #7573's C publishers an 8 s catalog
`delay`, and the convenience it buys has no use case. This replaces the
2026-10-27 audit's "producer arguments keep an optional timestamp", which
recorded only sequencing.

- **Required timestamps.** Every producer write in moq-net and every binding
  takes a timestamp. There is no `None`, and nothing is wall-clock stamped.
  Reads return a timestamp too.
- **Scale-less tracks.** A track with no timescale (moq-transport without
  TIMESCALE, lite-01..04, or an app with no clock) is scale-less. Its frame
  timestamp is its group's key, and the write API on a scale-less group takes
  no per-frame time. Max-age on it counts bare units, so `max-age=5` keeps
  groups within 5 IDs of the newest; the application knows how sparse its
  IDs are.
- **One unit per track.** A track's timescale is the unit of both its frame
  timestamps and its group keys. Legacy keys are assumed to be in that unit
  and nothing checks it, so time ranges compare keys numerically. A released
  publisher numbering groups 0, 1, 2 with ms frames gets ranges in ID units,
  which is accepted. Max-age keeps comparing frame timestamps, as today.
- **Legacy IDs pass through.** Ingest keys a group by the wire's group ID on
  every legacy wire, and egress writes the key back, so moq-transport group 6
  leaves as group 6 and reaches moq-lite as key 6. No group carries a sequence
  beside its key.
- **Broadcasts are all timed or all scale-less.** moq-mux's catalog producer
  stamps the catalog from the broadcast clock and refuses to list a track off
  that clock's timescale, or a scale-less track beside timed ones. moq-net
  tracks stay independent.
- **Open, for whoever starts this, with the maintainer:** how lite-05/06,
  which carry both a sequence and frame timestamps, map on the way out.

Decision prompts:

- Ingest key: ✅ ID verbatim; first PTS, renumbered; per wire.
- A sequence beside the key: ✅ no, one key; yes, both.
- Timescale meaning: ✅ one unit for keys and frames, no legacy flag; flag
  legacy keys.
- Enforcing timed broadcasts: ✅ the catalog producer; moq-net broadcast;
  convention only.
- A scale-less frame's timestamp: ✅ its group key; any increasing number.
- Max-age on a scale-less track: ✅ bare units; doesn't apply.

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
`Position`, `Range` ends, `fetch_group`, `Subscriber::floor`, and group consumers
address by start timestamp, every producer write takes a required
timestamp, and untimed tracks become scale-less.
Wire: no format change; legacy and lite-07-wip group IDs become start ticks.

## Required

- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - the lite cursor state lives in the model before the cut rekeys it
- [Drop set](/quest/m1/drop-set.md) - its dropped and expired runs convert mechanically from sequences to starts
