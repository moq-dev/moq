# moq-net track cache (spike)

> Status: design spike, no implementation yet. Targets `dev`: it removes a public/wire field
> (`TrackInfo.cache`) and adds local API to `TrackProducer`.

A per-track group cache owned by a `TrackProducer`. It lets a relay or edge retain recent
groups past the live window and serve them back on a FETCH, optionally spilling to local disk
or remote object storage. This is the moq-net mechanism the `moq-archive` crate builds on (see
`../moq-archive/DESIGN.md`); the on-tier byte format is shared with that crate.

## Principles

These come from design review and pin down the shape:

- **Owned by `TrackProducer` only.** The cache is local policy set by whoever holds the
  producer (the relay or edge), never by the original publisher and never carried on the wire.
  This is why `TrackInfo.cache` goes away (see "Removing TrackInfo.cache").
- **Per-track bounds, no shared LRU.** Each track keeps a `[min, max]` window of its own recent
  groups. There is no cross-track accounting, so no shared lock and no contention. The cost is
  that there is no global RAM ceiling; total footprint is the sum of per-track `max` across
  live tracks. A global backstop, if ever needed, is additive and not part of v1.
- **No traits, no callbacks.** `Cache` is a concrete value you configure and attach. moq-net
  owns all behavior. The disk and remote backends are an internal, configured `object_store`,
  not a consumer-implemented extension point.
- **Watermark flush, not per-item eviction.** Groups accumulate to the high watermark, then a
  whole band (the `max - min` worth) is flushed as one segment. This is the property an LRU
  cannot provide: an LRU evicts one group the instant the budget trips, producing one tiny
  object per group, which is fatal for audio (a group per frame). The watermark is what creates
  batches.

## Bounds

Per track, on both size and duration, whichever trips first:

```rust
/// Local cache policy for a single TrackProducer. Not on the wire, not in TrackInfo.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct CacheConfig {
    pub ram: Bounds,                // keep >= min in RAM; flush the band once > max
    pub disk: Option<Tier>,         // local object_store + its own Bounds
    pub remote: Option<Tier>,       // remote object_store + its own Bounds
    pub interval: Option<Duration>, // max wall-clock before a partial band flushes anyway
}

/// A low/high watermark. The gap (max - min) is the flush batch size.
pub struct Bounds { pub min: Limit, pub max: Limit }

/// A bound expressed as a duration, a byte count, or both (first to trip wins).
pub struct Limit { pub duration: Option<Duration>, pub bytes: Option<u64> }

/// A persistent tier: an object_store plus its own retention bounds.
pub struct Tier { pub store: /* path or url */ (), pub bounds: Bounds }
```

The flush batch is implicitly `max - min`, so the bounds map straight onto the tiering the
archive doc describes:

| Want | Set |
|---|---|
| keep 30s in RAM, flush 10s segments to disk | `ram.min = 20s`, `ram.max = 30s` |
| keep 5m on disk, flush 1m objects to remote | `disk.min = 4m`, `disk.max = 5m` |

At 30s the buffer drains back to 20s, emitting a 10s segment, then refills over the next 10s.
No explicit batch size: the band is the batch.

`interval` is a backstop so a low data-rate track still flushes eventually instead of holding a
half-full band for a long time. A duration-based `max` already covers most of this (the oldest
group ages past `max` even with little data), so `interval` matters chiefly when the bounds are
byte-only.

## State and flush

Each track owns a small buffer plus an index of what has been flushed where:

```rust
struct TrackCache {
    ram: VecDeque<CachedGroup>,        // recent groups, ordered by sequence
    ram_bytes: u64,
    flushed: BTreeMap<u64, Location>,  // sequence -> (tier, object key, offset) for serving
    last_flush: Instant,
}
```

Flush runs on group completion and on a timer:

```text
if over(ram.max)  ||  ram.interval elapsed with a flushable band:
    batch = drain oldest completed, unpinned groups until back to ram.min
    match disk:
        Some(d) => segment = serialize(batch)              // archive segment format
                   d.put(key, segment)
                   for g in batch { flushed[g.seq] = Disk(key, offset) }
        None    => drop(batch)                             // RAM-only cache: just evict
// the disk tier runs the same watermark loop against disk.max, concatenating several
// small segments into one larger remote object (the rollup) and updating `flushed`.
```

## Serving

```text
get(seq):
    if let Some(g) = ram.find(seq)    -> serve(g)              // RAM hit, pin while read
    if let Some(loc) = flushed.get(seq) -> stream_from_tier(loc)  // ranged GET, no fault-back
    else -> None                                              // miss: upstream / Unroutable
```

A lower-tier hit streams straight from disk or remote via a ranged read. There is no fault-in
and no re-population of RAM, so a group lives in exactly one tier and is served from there. This
is what makes the watermark model simpler than an LRU, which needs to move items back up on
access.

## Always-latest and pinning

- **The latest group is never evicted.** It sits inside `ram.min` by construction, so this is
  free, and it is the group a new subscriber needs first.
- **A pinned (actively read) group is never flushed.** A `GroupConsumer` handed out from the
  cache holds a pin (hooked into the group's existing refcount); the flush skips pinned groups
  and emits the rest of the band. Old groups are rarely pinned, so segments stay contiguous in
  practice. If strictly contiguous segments are ever required, hold the batch until the pin
  clears instead.

## Tiers

RAM is always present and dependency-free. disk and remote are `object_store`, behind a
`cache-tiered` feature flag so RAM-only native builds (and any wasm consumers) do not pull the
cloud stack. The on-tier bytes reuse the `moq-archive` segment plus manifest format, so the
cache and the archive crate agree byte-for-byte and a relay's spilled data is directly readable
by an archive node.

## Integration with TrackProducer / TrackState

Today `TrackState.groups` is the inline per-track cache, bounded by the `TrackInfo.cache`
duration. With a `CacheConfig` attached:

- finished groups beyond `ram.min` move from the inline buffer into the cache's RAM tier;
- a `get_group` or `dynamic()` miss consults the cache (RAM, then disk, then remote) before
  failing with `NotFound`;
- nothing reads `TrackInfo.cache` any more.

The attach point is one local method:

```rust
impl TrackProducer {
    /// Attach a local cache. Retains and serves groups per `config`, independent of any
    /// retention the original publisher set.
    pub fn with_cache(self, config: CacheConfig) -> Self;
}
```

## Removing TrackInfo.cache

`TrackInfo.cache` is a producer-set, wire-serialized duration. It conflates "how long the
publisher keeps groups for late subscribers" with "cache policy," and a relay should not
inherit the publisher's number to size its own cache. Since the cache here is local and fully
independent:

- stop using `TrackInfo.cache` to size anything;
- remove the field from `TrackInfo`. This is a public-API and wire change, hence the `dev`
  target. If a producer-side retention knob is still wanted, it stays internal to the producer
  rather than on the shared `TrackInfo`.

## Per-binary use

- **moq-cli:** no cache, or a small RAM-only `CacheConfig` for a single track.
- **moq-relay:** one `CacheConfig` template applied to every track it creates. Threading that
  config onto the tracks moq-net auto-creates during fan-out is the Origin follow-up; here it is
  just `TrackProducer::with_cache(config)`. A relay RAM cache that spills to disk or S3 becomes
  configuration, not code.
- **moq-edge:** the same, plus its own dynamic-handler business logic on top.

## Open questions

1. **object_store in moq-net.** Feature-gate `cache-tiered`; RAM-only stays dependency-free.
   This is the one heavy dependency decision, since moq-net is the core wire crate.
2. **Async get.** RAM hits must stay synchronous (serve under the lock); only disk and remote
   faults are async. The return type needs a "ready now or pending" shape, matching moq-net's
   existing `kio::Pending`.
3. **Default bounds.** With `TrackInfo.cache` gone, pick a conservative RAM-only default so an
   unconfigured `TrackProducer` behaves like today: a small recent window, no spill.
4. **Footprint.** Per-track bounds mean total RAM is the sum of `ram.max` across live tracks.
   Keep the default modest and document footprint = bound times track count.
5. **Pinned groups mid-band.** Skip and flush around them, or hold the batch until unpinned.
   Skipping is simpler and old groups are rarely pinned; revisit only if it bites.
