# [L] kio append-only log

## Goal

kio exports an append-only log channel whose consumers read committed entries
by cursor without taking a lock, and the hottest moq-net channels the
[epic](/quest/m2/perf/kio-channel-contention/README.md)'s baseline names
(expected: `TrackState` arrival order, and `GroupState`'s tail run, which
[Front eviction](/quest/m1/unbounded-groups/front-eviction.md) lays out for
it) move onto it.
Lands with the contended bench (`rs/kio/benches/contended.rs`) and `just bench-profile lock` before and after; a
measured no-win abandons it.

## Plan

- First measure a multi-frame (video-heavy) shape with `just bench-profile lock`,
  since the baseline's chat and fanout shapes both send one-frame groups and
  leave `GroupState` frame contention unmeasured. Choose the target channels
  from that.
- Fan-out readers already read by cursor into an append-only log and clone
  one cheap item out (`Bytes`, `FrameBuf`, `group::Producer`). The mutex
  serializes every one of N woken readers; an atomic committed length lets
  readers below it proceed in parallel.
- Public in kio (decided 2026-10-14), so the atomic protocol stays in one
  crate under its loom harness; moq-net is the consumer that justifies the
  export. Propose the name with options in the PR.
- Appends never move a committed entry (segmented storage), are published
  with a Release store of the length, and wake parked readers. Writers
  serialize among themselves. A reader parks only when its cursor reaches the
  length, registering and then re-checking the length.
- Removal from the front is first-class (decided 2026-10-14), since most
  log-shaped moq-net state evicts its oldest entries. Entries carry absolute
  indices; eviction advances an atomic base, and a reader whose cursor fell
  below the base learns it skipped ahead rather than reading a stale entry.
  `GroupState::release()` is eviction of everything; `TrackState` pops its
  front.
- Prefer a maintained crate for the storage (e.g. `boxcar`) if it supports
  front eviction; hand-roll only what it can't.
- Open: reclaiming evicted entries while readers may still hold them.
  Recommended direction: reference-counted segments, so a reader's handle
  keeps its segment alive and eviction frees by drop, with no epoch scheme.
  Settle it first, with options and a recommendation.
- Scope is the primitive plus the two hottest channels, to prove it where the
  contention is. The rest of moq-net's log-shaped state is
  [Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md).
- Writes that are not appends (`TrackState` demote/evict, the scalar fields
  beside the log) stay on the channel's existing state; only the log moves.
- Waits on the spikes (decided 2026-10-14): with per-thread sessions every
  consumer of a replica is on one thread, so its mutex is uncontended and a
  lock-free multi-reader log only pays off if the shared model survives.
  Across threads the log becomes a flow-controlled byte stream, single writer
  and single reader, which the
  in-memory transport (`moq_net::transport::memory`)
  already provides.
- Breaking moq-net's model internals is allowed; check whether any public
  moq-net API changes and sync `js/net` if so.
- The group storage benchmark
  ([moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627), numbers in
  the [epic](/quest/m2/perf/kio-channel-contention/README.md)) prototyped
  this log for `GroupState`: 10 to 40% less per reader than the mutex across
  cores at 1,024 readers, within 15% on one thread, and about 15 ns more per
  append. A ring buffer was rejected. Recommended, pending the maintainer:
  Front eviction lands the layout (a tail run from an advancing base, refills
  in runs below it) under the mutex, and this replaces only the tail run.
  Refills stay in their own locked runs, since they are written out of order.
- From the prototype: segments doubling from 4 to 64 slots keep a one-frame
  group as cheap as today. Linking a segment to the next by `Weak`, with the
  log holding the strong references, bounds a reader that stopped polling to
  pinning its own segment, not every one after it. Those pinned bytes are
  still uncharged to the cache pool once evicted.
- Parking still takes the waiter mutex, which is now the edge reader's only
  acquire; the
  [version counter](/quest/m2/perf/kio-channel-contention/version.md)'s
  split waiter lists are the lever there.

## Required

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - lays out `GroupState`'s tail run and refill runs, so this replaces only the tail run's storage
- [Replica spike](/quest/m2/perf/kio-channel-contention/spike-replica.md) - if per-worker replicas win, each channel's cross-core readers collapse to one flow-controlled stream per worker and this log is likely abandoned
- [Cluster spike](/quest/m2/perf/kio-channel-contention/spike-cluster.md) - the same question with nothing shared
