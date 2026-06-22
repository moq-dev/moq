# moq-net track cache

A per-track durable cache. It lets a relay or edge keep recent groups past the live window and
serve them back on a FETCH, spilling to local disk and optionally remote object storage. It lives
in `moq-net` so any consumer of a track (relay, edge, archiver) gets durable caching for free.

## Shape

The cache is **not** a separate handle you wire onto both endpoints. It lives on the shared track
state (`TrackState`), so the RAM tier is the track's own live `groups` buffer and the disk/remote
tiers hang off the same state. One store therefore backs the track's `TrackProducer` and every
`TrackConsumer` automatically; a fetch is served from whichever tier holds the group.

```rust
// module moq_net::cache (native-only types are target-gated to non-wasm)

let disk = cache::Disk::new(store, prefix, bounds)         // object_store + key prefix + bounds
    .with_remote(remote);                                  // optional rollup target

let producer = TrackProducer::new(name, info).with_cache(disk);
let consumer = producer.consume();                         // shares the same store
```

## Principles

- **Local, not on the wire.** The cache is local policy set by whoever holds a track endpoint (the
  relay or edge), never by the original publisher and never carried on the wire.
- **RAM is the live window.** There is no second in-memory copy of recent groups: the cache reuses
  `TrackState.groups`, the buffer the track already keeps for live subscribers. A group is
  serialized (to `cache::Group`) and handed to the disk tier only when it ages out of that window.
- **No traits, no callbacks.** The cache is concrete values you configure and attach. moq-net owns
  all behavior; the disk and remote backends are a configured `object_store`, not a
  consumer-implemented extension point.
- **Per-track, no shared LRU.** Each track keeps its own recent window; there is no cross-track
  accounting, so no shared lock. Footprint is the sum of per-track windows across live tracks.

## Retention: two gates

A group is evicted from the live window (`TrackState::evict_expired`) when it trips **either** of
two gates, both sized by `TrackInfo::cache` (the publisher's retention duration). The newest group
(`max_sequence`) is never evicted.

- **Wall-clock** — the group was *received* more than the window ago. The receive time is an
  `Instant` stamped when the group lands in `groups`; it is never sent over the wire or set by the
  publisher. This is the hard memory backstop: a publisher can't pin RAM by lying about media
  timestamps.
- **Media-time** — the group's last frame timestamp is more than the window behind the live media
  edge (the newest frame timestamp buffered). This bounds a startup stampede, where a burst of
  buffered media arrives at once (all "received now", so the wall-clock gate alone would keep it
  all) and a fresh subscriber would otherwise be flooded.

In steady state, where media time advances with wall-clock time, the two gates coincide. They
diverge only under a stampede (media-time trims it) or timestamp abuse (wall-clock trims it).

## Spill and serve

```text
evict_expired:                              (synchronous, under the state lock)
    for each group outside the window (not max_sequence):
        tombstone it in `groups`
        if a cache is attached: hand its live GroupConsumer to the flush task

flush task:                                 (one background task per cached track)
    per eviction pass: drain the groups into cache::Group, write ONE disk segment,
    then compact (roll the oldest disk segments up into one remote object, or evict
    them when there is no remote tier)

fetch_group(seq):
    live hit in `groups`                 -> serve immediately
    live miss, cache attached            -> spawn an async disk/remote lookup; a hit
                                            resolves the fetch, a miss chains upstream
                                            (queues for a TrackDynamic), else NotFound
    live miss, no cache                  -> queue for a TrackDynamic, or NotFound
```

`get_group(seq)` stays synchronous and only consults the live window; a spilled group is reachable
only through the async `fetch_group`.

On a tier miss the lookup task chains upstream: it queues the request for a `TrackDynamic` (a wire
FETCH for a relay) when one exists, so the fetch then resolves once upstream serves the group into
the live window. Queuing only *after* the store misses keeps the store the fast path and avoids a
redundant upstream fetch when the group is already cached. With no handler, a miss is `NotFound`.

Batching the disk write per eviction pass keeps a stampede-trim (many groups evicted at once) to a
single object. A steady-state single eviction still writes one small disk segment per group; the
remote tier is where rollup (`segment::rollup`) concatenates those into large objects, so a
per-frame (audio) track does not litter object storage with tiny remote objects.

## Tiers and the byte format

RAM is always present and dependency-free. Disk and remote are `object_store`, target-gated to
non-wasm targets (`cfg(not(target_arch = "wasm32"))`) so native builds get the tiers with no flag
and wasm builds drop the server-side cloud stack automatically.

The on-disk format lives in `segment.rs`: a band of groups serialized as one self-describing
object (a footer offset table read from a fixed trailer), lossless per-frame timestamps (raw
value + scale, so any timescale round-trips), `rollup` to concatenate small segments into one
larger object, and `group_from_blob` for the ranged-read decode path. `index.rs` is the
storage-agnostic multi-tier index (`sequence -> (tier, segment, byte range)`), per-tier byte and
duration accounting, and the promotion that picks the oldest disk segments over the disk high
watermark. `store.rs` is the `object_store` glue tying them together. The disk `Bounds` (a
low/high watermark) govern when disk segments roll up to remote, independent of the RAM retention
window above.

## Bridging live <-> cached

`cache::Group::read` drains a finished live `GroupConsumer` into the serializable `cache::Group`
(done on the flush task, off the state lock). `cache::Group::produce` rebuilds a live
`GroupConsumer` from a stored group at the track's timescale, for serving a fetch.

## Still design

- **Removing `TrackInfo::cache`.** The retention window is still read from the wire-carried
  `TrackInfo::cache`. Making retention purely local policy (and dropping the wire field) is a
  separate wire change.
- **moq-cli / moq-relay flags.** Surfacing `with_cache` as CLI/TOML configuration (a disk path, a
  remote URL, bounds) is follow-up work; the model API is in place.
