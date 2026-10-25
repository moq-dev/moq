# Unbounded groups and frames

## Goal

Groups and frames have no fixed size cap. `MAX_GROUP_FRAMES` (8192) and
`MAX_CACHE_BYTES` (32 MB, which doubles as the frame cap) go away, and the
shared cache pool alone bounds memory. Under pressure, the cache sheds parts
of a group and refills the holes later from upstream, or from the
publisher's own refill handler. Readers never fail just because a group got
big. The motivating workloads are long-lived single groups (data/log tracks,
audio, window timelines), video with very long GOPs, and file transfers where
a frame is a whole file.

The hot write and read path for ordinary groups does not regress.

Non-goals: pinning memory for slow readers, and byte-precise refill over
moq-transport.

## Plan

Decided 2026-10-23 (maintainer): moved from m2 to m1, holding the next
breaking cut, so a long-lived log such as a moq-json `Stream` has no record
ceiling in that release. Making `Stream` roll groups instead was rejected:
its readers reject a second group (`Rolled`), and a `Window` that never pops
is already the rolling log. Pointing `Stream`'s docs at `Window` in the
meantime was declined.

Decided 2026-10-24 (maintainer interview): one group should work as an
indefinite stream, since many applications are 1:1 and keeping a whole
group just in case someone joins is wasteful. Refills ask for exactly the
hole and are unpaced; reader-paced refill "trains" that merge were rejected
as not worth their complexity. A refill refreshes the frames after it so it
never evicts what its reader needs next. When nothing serves a hole, the
reader gets `Evicted` and a joiner falls back to `Info::refresh`. The live
edge is never paced by readers. Groups are bounded by bytes, with each
frame's slot charged to the pool. Details are in
[Front eviction](/quest/m1/unbounded-groups/front-eviction.md).

Decided 2026-10-16 (maintainer interview):

- #3585 replaced front eviction with a `GroupTooLarge` abort because a reader
  that kept up streamed the whole group while a late one got `Lagged`. Refill
  removes that inconsistency: a late reader refills instead of failing.
- Prior art (nginx slice, Varnish, ATS, Squid, Fastly/Akamai segmented
  caching, Chromium sparse entries, pingora-cache) splits two ways:
  - Append-and-follow storage (Varnish, ATS, Pingora) streams an object
    whose length is unknown, but evicts it only whole.
  - Fixed slices (nginx slice, Fastly) evict independently, but need a known
    total length.

  No surveyed cache keeps head and tail of an in-progress object while
  evicting its middle. MoQ can, because a name always means the same
  content: there are no validators, and runs never need to be stitched by
  version.
- Holes and refills are addressed by `(frame, byte)` positions, not
  group-wide byte offsets. Small frames never use the byte. Route
  failover's `Recover` continues a half-read frame at its byte, but it
  refetches the frame from its start and skips locally, so a byte-precise
  request is new work.
- Large frames become chains of sealed segments. Small frames keep a single
  `Bytes`.
- QUIC receive credit is released per segment, so an arbitrary-size frame
  never needs a matching receive allowance. The aim is to avoid
  over-allocating, not to bound a frame.
- Refills patch the group's state in place, written only below the tail
  run's base so they never race the tail writer. They replace today's
  separate `Slot.head` copy.
- Storage (decided 2026-10-24 by the maintainer, over the group storage
  benchmark, [moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627)):
  the lock-free append-only log (`kio::log`) is the
  tail run, from a base that only advances, and refilled runs sit below it.
  It moved here from m2, ahead of Front eviction, so the end-state storage
  lands once instead of a mutex layout swapped later. Prototyped, the log
  cost 10 to 40% less per reader across cores at 1,024 readers and about
  15 ns more per append than today's `VecDeque`. A ring buffer was rejected:
  an uncapped group has no size to give it. Numbers are in the
  [kio contention epic](/quest/m2/perf/kio-channel-contention/README.md).
- Readers pin nothing: reads refresh recency as today. A reader that
  falls into an evicted run refills like a joiner. With nothing to refill
  from, it gets `Error::Evicted`. Within a group, victims go front to back.
  Keeping the head for joiners while evicting the middle moved to m3
  ([Middle eviction](/quest/m3/middle-eviction.md), decided 2026-10-20).
- Whole cold groups are still evicted first. A group loses part of itself
  only when it is the protected live edge or is being read.
- Over moq-transport, an object with a hole fails the reader with `Evicted`
  rather than refetching. Only whole-object holes refill.
- `GroupTooLarge` and `FrameTooLarge` stay, raised only when an allocation
  can't be satisfied.
- The byte offset goes on the next unpublished lite version after lite-07,
  not lite-07. Frame-boundary holes refill over lite-07's frame offsets once
  [transport frame offsets](/quest/m1/transport-frame-offsets.md) lets a
  refill start at a frame the relay no longer holds below (decided
  2026-10-25, maintainer: the application API stays whole-group).
- JS is a separate quest. Docs change inline: the lite draft, `doc/concept`,
  the relay cache docs, the JSON and moq-net compression docs, and the e2ee
  draft's 32 MiB interop cap. The
  [m1 compression migration](/quest/m1/compress/migrate.md) deletes the flate packages. There is no new page.

The refill API is the `track::Dynamic` fill (`track::Fill`). A handler serves a range by writing into a
group producer bounded to that range, the same API as a live write, so an
origin can regenerate data from disk or an archive.

This epic owns the end-to-end test: a relay chain carries one live group well
past the old caps through a pool far smaller than the group. A late joiner
gets its head through refill and a live reader keeps the tail, over lite and
moq-transport, with memory held near the pool size.

## Required

- [Segmented frames](/quest/m1/unbounded-groups/segmented-frames.md) - large frames store as sealed segments, preallocate at most one, and release receive credit per segment
- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - groups shed whole frames from their front under pressure and refill them in place; the group caps go
- [Byte holes](/quest/m1/unbounded-groups/byte-holes.md) - segments inside a frame evict and refill by `(frame, byte)` on the next lite version; the frame cap goes
- [JS unbounded groups](/quest/m1/unbounded-groups/js.md) - `@moq/net` drops its caps, streams huge frames, and matches front eviction
- [Log lock profile](/quest/m1/unbounded-groups/log-lock-profile.md) - a lock profile on a perf host confirms the epoch-based `kio::log` cut `GroupState` contention

## Related

- [Middle eviction](/quest/m3/middle-eviction.md) - a group keeps its head for joiners and its tail for live readers, evicting between
