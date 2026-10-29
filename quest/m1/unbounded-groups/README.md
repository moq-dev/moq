# Unbounded groups and frames

## Goal

Groups and frames have no fixed size cap. `MAX_GROUP_FRAMES` (8192) goes
away, and `MAX_CACHE_BYTES` (32 MB) stops capping groups and frames and
instead bounds how much of a group's live tail is held. A group sheds whole
frames from its front once its tail passes 32 MB, or sooner when the shared
cache pool is over budget, so one long group never grows without bound even
on a relay with no cache capacity set. A reader or FETCH that reaches a shed
front is served by a flight, a separate paced copy filled from upstream or
the publisher's own refill handler; failing that, it asks for a new group
(refresh), and otherwise it gets `Evicted`. Readers never fail just because a group got big. The motivating
workloads are long-lived single groups (data/log tracks, audio, window
timelines), video with very long GOPs, and file transfers where a frame is a
whole file.

The hot write and read path for ordinary groups does not regress.

Non-goals: pinning memory for slow readers, byte-precise refill over
moq-transport, and starting a reader mid-group at whatever the cache still
holds (a tail join).

## Plan

Decided 2026-10-28 (maintainer, after an audit of the plan and
[moq-dev/moq#6993](https://github.com/moq-dev/moq/pull/6993)):

- A group sheds its front once its live tail passes 32 MB
  (`MAX_CACHE_BYTES`), not only under pool debt. The relay's default pool has
  no byte capacity and the latest group is exempt from idle expiry, so
  shedding on pool debt alone let one live group grow without bound once the
  group caps went. The 32 MB counts a group copy's live tail (its
  `kio::log` run), and nothing is kept below it: no head, no refilled runs.
- With [fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md),
  the 32 MB is one budget split between "ahead" (bytes the fastest reader has
  not read, bounded by pacing) and "behind" (bytes it has read, kept for
  slower readers). Shedding never passes the fastest reader, and the ahead
  window is at most 16 MB, so a reader within the behind half of the
  fastest one keeps up and a slower one falls off.
- A reader at a shed frame, a late joiner or one that fell off, is served by
  a [flight](/quest/m1/unbounded-groups/flights.md) when the publisher has a
  refill handler: a separate copy with its own upstream fill and 32 MB tail,
  paced by its fastest rider. A reader joins a flight only within 16 MB of
  its fastest rider, and hands over to the copy ahead at the same frame once
  it gets within 16 MB of that copy's fastest rider. Without a handler it
  asks for a new group on tracks that opt into refresh, else gets `Evicted`.
  This replaces in-place refill into runs below the log, and reopens
  2026-10-24's rejection of paced trains.
- No tail join, opt-in or otherwise: a reader never starts at an arbitrary
  offset inside a group. Joiners resync at a group boundary, by a flight, or
  by refresh, where the publisher starts a new group with a fresh snapshot
  or keyframe (delta compression), as HLS joiners start at a segment.
  Keeping a group's head for joiners (middle eviction) was dropped for the
  same reason.
- Compressed groups are not special: they shed under the same rules as
  plain groups, with no separate budget or carve-out.
- Prior art splits the same way: Varnish streaming, ATS read-while-writer,
  and WHATWG `tee()` fan out at the writer's pace and buffer everything;
  Varnish `transit_buffer` and nginx unbuffered proxying pace one client;
  HLS and DASH cut a stream into finite segments joiners start at. This epic
  is `tee()` with a bounded buffer, paced on the fastest branch.

Decided 2026-10-23 (maintainer): moved from m2 to m1, holding the next
breaking cut, so a long-lived log such as a moq-json `Stream` has no record
ceiling in that release. Making `Stream` roll groups instead was rejected:
its readers reject a second group (`Rolled`), and a `Window` that never pops
is already the rolling log. Pointing `Stream`'s docs at `Window` in the
meantime was declined.

Decided 2026-10-24 (maintainer interview): one group should work as an
indefinite stream, since many applications are 1:1 and keeping a whole
group just in case someone joins is wasteful. (Its unpaced exact-hole
refills, and its rejection of reader-paced trains that merge, were
superseded 2026-10-28 by flights.) When nothing serves a hole, the reader
gets `Evicted` and a joiner falls back to `Info::refresh`. The live
edge is paced by its fastest reader (decided 2026-10-25, superseding
2026-10-24's "never paced by readers"; see
[Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md)), so a
byte stream over MoQ loses nothing to a reader that keeps up. Groups are bounded by bytes, with each
frame's slot charged to the pool. Details are in
[Front eviction](/quest/m1/unbounded-groups/front-eviction.md).

Decided 2026-10-16 (maintainer interview):

- #3585 replaced front eviction with a `GroupTooLarge` abort because a reader
  that kept up streamed the whole group while a late one got `Lagged`. A
  flight removes that inconsistency where the publisher allows replay, and
  refresh resyncs a late reader where it doesn't.
- Prior art (nginx slice, Varnish, ATS, Squid, Fastly/Akamai segmented
  caching, Chromium sparse entries, pingora-cache) splits two ways:
  - Append-and-follow storage (Varnish, ATS, Pingora) streams an object
    whose length is unknown, but evicts it only whole.
  - Fixed slices (nginx slice, Fastly) evict independently, but need a known
    total length.

- Holes and flight starts are addressed by `(frame, byte)` positions, not
  group-wide byte offsets. Small frames never use the byte. Route
  failover's `Recover` continues a half-read frame at its byte, but it
  refetches the frame from its start and skips locally, so a byte-precise
  request is new work.
- Large frames become chains of sealed segments. Small frames keep a single
  `Bytes`.
- QUIC receive credit is released per segment, so an arbitrary-size frame
  never needs a matching receive allowance. The aim is to avoid
  over-allocating, not to bound a frame.
- Storage (decided 2026-10-24 by the maintainer, over the group storage
  benchmark, [moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627)):
  the lock-free append-only log (`kio::log`) is a group copy's
  tail run, from a base that only advances.
  It landed in [moq-dev/moq#6782](https://github.com/moq-dev/moq/pull/6782)
  (d854bf98b1), ahead of Front eviction, including `log::Writer::evict`
  advancing the base (`rs/kio/src/log.rs`). A ring buffer was rejected: an
  uncapped group has no size to give it. Numbers are in #6782 and the
  [kio contention epic](/quest/m2/perf/kio-channel-contention/README.md).
- Readers pin nothing: reads refresh recency as today. Within a group,
  victims go front to back.
- Whole cold groups are still evicted first. A group loses part of itself
  only when it is the protected live edge or is being read.
- Over moq-transport, a flight starts only at an object boundary; a reader
  inside a shed object refreshes or gets `Evicted`.
- `GroupTooLarge` and `FrameTooLarge` stay, raised only when an allocation
  can't be satisfied.
- The byte offset goes on the current lite wip version (lite-07-wip today);
  the lite-07 cut does not wait on it. Decided 2026-10-27 (maintainer): wire
  quests target whichever wip is current when they land, so
  [Byte holes](/quest/m1/unbounded-groups/byte-holes.md) waits only on Front
  eviction and Flights. If lite-07 is cut first, lite-07 peers start flights
  only at frame boundaries, over lite-07's transport frame offsets, which
  allow a fill to start without the shed prefix (decided 2026-10-25,
  maintainer: the application API stays whole-group).
- JS is a separate quest. Docs change inline: the lite draft, `doc/concept`,
  the relay cache docs, the JSON and moq-net compression docs, and the e2ee
  draft's 32 MiB interop cap. The
  [m1 compression migration](https://github.com/moq-dev/moq/pull/7046) deleted the flate packages. There is no new page.

A flight's fill is the `track::Requests` fill (`track::Fill`). A handler serves a range by writing into a
group producer bounded to that range, the same API as a live write, so an
origin can regenerate data from disk or an archive.

This epic owns the end-to-end test: a relay chain carries one live group well
past the old caps. A late joiner gets its head through a flight and hands
over to the live copy, and a live reader keeps the tail, over lite and
moq-transport, with memory held near 32 MB per copy.

## Required

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - groups shed whole frames from their front past a 32 MB tail or under pool debt; the group caps go
- [Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md) - every hop buffers at most a window past its fastest reader and pushes back through QUIC flow control
- [Flights](/quest/m1/unbounded-groups/flights.md) - a reader at a shed frame replays through a separate paced copy that hands over to the copy ahead
- [Byte holes](/quest/m1/unbounded-groups/byte-holes.md) - segments inside a frame shed like front frames and flights start at `(frame, byte)` on the current lite wip; the frame cap goes
- [JS unbounded groups](/quest/m1/unbounded-groups/js.md) - `@moq/net` drops its caps and matches front eviction
- [JS byte holes](/quest/m1/unbounded-groups/js-byte-holes.md) - `@moq/net` reads a huge frame as chunks and mirrors the byte-offset wire
- [Log lock profile](/quest/m1/unbounded-groups/log-lock-profile.md) - a lock profile on a perf host confirms the epoch-based `kio::log` cut `GroupState` contention
