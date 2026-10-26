# [L] Spike: timestamps replace group sequence numbers

## Goal

A throwaway prototype in Rust moq-net and `@moq/net` drops group sequence
numbers from the API and the lite-07 wire, addressing each group by its start
timestamp, and measures what breaks. The spike ends by recording the design
in new implementation quests, adding them to
[Finalize moq-lite-07](/quest/m1/lite07-finalize.md)'s Required list and the
[guide](/quest/m1/time-addressing/guide.md)'s, and deleting itself. The
prototype is not merged.

## Plan

Decided 2026-10-25 (maintainer). Why: group numbers need timelines just to
map to and from time, and reusing a group number poisons caches, so resuming
a producer would need per-track memory of its last number.

- **API and wire.** Sequence numbers leave the moq-net API and the lite-07
  wire, not just the API. This gates the lite-07 cut, since lite-07 is still
  wip.
- **A group's address is its first frame's PTS**, known when the group opens,
  so the GROUP header and legacy IDs can carry it. Frames inside a group stay
  in decode order, and their PTS may go backwards (B-frames). Check that a
  group's end and reach use its highest PTS, not its newest frame's
  (`rs/moq-net/src/model/group.rs:2022`).
- **Starts strictly increase.** An equal start is refused loudly like a
  rewind; today the rewind check allows ties (`track.rs:491`).
- **Three ways a group ends**, mirrored in the API and lite-07:
  `finish(end)` sends GROUP_END with an exclusive end timestamp, meaning the
  group is whole; `close()` is a bare FIN, meaning an incomplete group whose
  frames all arrive (better than a reset, which drops data in flight); and
  `abort(err)` resets. So a relay closes an expired or partial group instead of
  resetting it (lite draft line 760 changes). GROUP_END maps to
  moq-transport's END_OF_GROUP. It replaces hang's empty-frame duration
  marker (`rs/moq-mux/src/container/producer.rs:523`), so moq-mux stops
  writing empty frames. Evaluate whether a finished group's own end can set
  its reach, so expiry stops waiting for a successor.
- **Discontinuity marker groups go away**
  (`rs/moq-mux/src/container/producer.rs:650`). A gap is a previous end below
  the next start; a missing group is a previous-start link that doesn't match.
  Legacy egress synthesizes neither the empty frame nor marker groups: old
  players lose end durations and pause markers, which they already tolerate
  being absent.
- **Restarts keep minting a new epoch** with fresh media PTS, as today.
  Replicas sharing an epoch agree by PTS without coordinating numbers.
- **Every track is timed.** Untimed goes away: the producer stamps an untimed
  write with wall-clock time.
- **Gaps.** Each GROUP carries the previous group's start, so a cache or a
  reader sees a hole without a round trip. The first group of an epoch has
  none, and the gap before it is unknown.
- **Legacy egress** (lite-01..06 and moq-transport): the group ID is the
  group's start in the track's timescale, so every relay agrees without a
  table. Verify that lite-05/06 receivers and published `@moq/net` and
  `@moq/watch` builds accept sparse, large IDs (JS numbers are exact to
  2^53).
- **Legacy ingest.** A timed source keeps its timestamps. An untimed source
  (moq-transport without TIMESCALE, lite-01..04) uses the group ID as the
  timestamp on a synthetic scale, so relays agree and legacy egress round-trips
  the exact ID. Max age means nothing on those tracks. A timed moq-transport
  group that starts at or before its predecessor ends the track with an
  error.
- **moq-transport mapping.** The Group ID is the start ticks, and the
  previous-start link rides in the Prior Group ID Gap property. Object IDs
  stay +1 per frame, so no Prior Object ID Gap is sent; each object's PTS
  rides in the TIMESTAMP property, and GROUP_END maps to END_OF_GROUP.
  Only draft-14 lets a status object carry properties, so only there does
  END_OF_GROUP carry the end in its TIMESTAMP; later drafts refuse them
  (`rs/moq-net/src/ietf/object.rs:57`), so their END_OF_GROUP stays bare and
  the end is lost unless an extension carries it. Keep it LOC compatible:
  TIMESTAMP (0x10) and TIMESCALE (0x08) are the MOQ Properties shared with
  draft-ietf-moq-loc. Rejected: Object ID as a PTS delta, because B-frames
  run backwards in decode order and open-GOP leading frames present before
  the group's start.
- **Extensions are optional.** moq-transport may carry the new information
  through properties or a negotiated extension, but moq-transport without
  them, and lite-01..06, must still work for live playback. Only end
  precision and gap detection degrade.
- **Ranges.** [Subscribe ranges](/quest/m1/subscribe-ranges/README.md)
  proceeds in parallel; the cut reworks its `Absolute(group)` ends and
  sequence-range SUBSCRIBE_DROP onto times. `Relative(n)` can stay, since it
  counts groups rather than naming them.
- **[Name reuse](/quest/m1/name-reuse.md)** lands as planned. The cut removes
  its sequence half, leaving a re-created track to keep its PTS moving
  forward.

The prototype covers the model, the lite-07 codec, and the legacy mapping in
Rust and JS. Every current role of the sequence needs a replacement or a
reason to drop it:

- The cache key, the duplicate check, and the hand-out-once record
  (`track.rs:318`, `delivered.rs`), plus slot stamps.
- Successor, reach, expiry, the arrival ledger, tail accounting, fills and
  misses, and SUBSCRIBE_DROP accounting, which all need a total order and a
  visible gap.
- Resume across route flips (`resume.rs`), mid-group `Position` and
  `frame_start`, priority order within a subscription, `Demand::wanted`, and
  refresh.
- Wire fields: GROUP, SUBSCRIBE and SUBSCRIBE_UPDATE bounds, SUBSCRIBE_START
  and its largest, SUBSCRIBE_END, SUBSCRIBE_DROP, datagrams (which share the
  group space), and every IETF Location, Filter, FETCH, Largest, and
  NEW_GROUP_REQUEST through the legacy mapping.
- Consumers that number groups: hang and moq-mux timelines, HLS/DASH export,
  `seek` for GOP-aligned failover in moq-mux and the bindings, HLS import's
  discontinuity packing, transcode's 1:1 numbering, archive keys and resume,
  relay and CLI `fetch --group`, and player continuity checks in the
  Rust and JS container consumers and `js/watch`.
- The E2EE nonce, which binds the group sequence
  (`drafts/draft-lcurley-moq-e2ee.md`, `rs/moq-e2ee`).

The implementation quests also update `doc/concept`, the lite, hang, and E2EE
drafts, every binding per the cross-package sync table, and `MIGRATE.md`.

Public API: none (prototype). Wire: none (prototype).

## Related

- [Lite-05/06 timestamps pass through](/quest/m1/lite-timestamps-through.md) - lite-05/06 tracks become timed, which this builds on
- [Resume checked against a model](/quest/m1/resume-model-test.md) - its reference model is the oracle for resume after the cut
