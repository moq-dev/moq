# [L] kio append-only log

## Goal

kio exports an append-only log channel whose consumers read committed entries
by cursor without taking a lock, and `GroupState`'s frames move onto it. It is
the tail run of the storage
[Front eviction](/quest/m1/unbounded-groups/front-eviction.md) builds on: a
log from a base that only advances, with refilled frames kept apart below
that base.
Lands with the contended bench (`rs/kio/benches/contended.rs`), a
`group.rs` bench, and `just bench-profile lock` before and after.

## Plan

- Decided 2026-10-24 (maintainer, group storage benchmark
  [moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627)): Front
  eviction lands on this log rather than on a mutex layout swapped later, so
  the end-state storage lands once. This moved from the m2
  [kio contention epic](/quest/m2/perf/kio-channel-contention/README.md) into
  m1, ahead of Front eviction. A measured no-win no longer abandons it; a
  before and after that contradicts the prototype goes back to the
  maintainer.
- No longer waits on the replica and cluster spikes. They decide whether
  cross-core readers survive, which only changes how much this wins, not the
  storage Front eviction needs. Their question still gates the rest of
  moq-net, so `TrackState` arrival order moved to
  [Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md),
  which keeps waiting on them.
- First run `just bench-profile lock video-heavy` as the "before": the
  epic's chat and fanout shapes both send one-frame groups and leave
  `GroupState` frame contention unmeasured. It needs
  `sudo sysctl -w kernel.perf_event_paranoid=-1` on the host.
- The prototype's numbers, in the
  [kio contention epic](/quest/m2/perf/kio-channel-contention/README.md), are
  the expected win: 10 to 40% less per reader than the mutex across cores at
  1,024 readers, within 15% on one thread, and about 15 ns more per append.
  The prototype is
  [`rs/moq-net/benches/storage.rs` at `2cfc0a6`](https://github.com/moq-dev/moq/blob/2cfc0a666dd79d4dbf068ffb27b8059ecb3019e7/rs/moq-net/benches/storage.rs).
- Fan-out readers already read by cursor into an append-only log and clone
  one cheap item out (`Bytes`, `FrameBuf`). The mutex serializes every one
  of N woken readers; an atomic committed length lets readers below it
  proceed in parallel.
- Public in kio (decided 2026-10-14), so the atomic protocol stays in one
  crate under its loom harness; moq-net is the consumer that justifies the
  export. Propose the name with options in the PR.
- Appends never move a committed entry (segmented storage), are published
  with a Release store of the length, and wake parked readers. Writers
  serialize among themselves. A reader parks only when its cursor reaches the
  length, registering and then re-checking the length.
- Removal from the front is first-class (decided 2026-10-14). Entries carry
  absolute indices; eviction advances an atomic base, and a reader whose
  cursor fell below the base learns it skipped ahead rather than reading a
  stale entry. `GroupState::release()` is eviction of everything.
- Prefer a maintained crate for the storage (e.g. `boxcar`) if it supports
  front eviction; hand-roll only what it can't.
- From the prototype: segments doubling from 4 to 64 slots keep a one-frame
  group as cheap as today. Linking a segment to the next by `Weak`, with the
  log holding the strong references, bounds a reader that stopped polling to
  pinning its own segment, not every one after it.
- Open: reclaiming evicted entries while readers may still hold them.
  Recommended direction: reference-counted segments, so a reader's handle
  keeps its segment alive and eviction frees by drop, with no epoch scheme.
  Those pinned bytes are uncharged to the cache pool once evicted; this
  quest owns charging or bounding them, since Front eviction relies on the
  pool to bound memory. Settle it first, with options and a recommendation.
- Writes that are not appends (the scalar fields beside the frames) stay on
  the channel's existing state; only the frames move.
- Parking still takes the waiter mutex, which is now the edge reader's only
  acquire; the
  [version counter](/quest/m2/perf/kio-channel-contention/version.md)'s
  split waiter lists are the lever there.
- Breaking moq-net's model internals is allowed; check whether any public
  moq-net API changes and sync `js/net` if so.

## Related

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - adds refilled runs below this log's base and sheds its front under pool pressure
- [Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md) - `TrackState` and the rest of moq-net's log-shaped state, after the spikes
