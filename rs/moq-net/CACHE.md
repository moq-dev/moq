# moq-net track cache (spike)

> Status: the RAM tier and eviction policy are implemented in `src/model/cache.rs` (module
> `moq_net::cache`, with unit tests). The disk/remote tiers and the `TrackProducer` /
> `TrackConsumer` wiring are still design. Targets `dev`: it removes a public/wire field
> (`TrackInfo.cache`) and adds local API to the track endpoints.

A per-track group cache. It lets a relay or edge retain recent groups past the live window and
serve them back on a FETCH, optionally spilling to local disk or remote object storage. This is
the moq-net mechanism the `moq-archive` crate builds on (see `../moq-archive/DESIGN.md`); the
on-tier byte format is shared with that crate.

The implemented surface follows moq-net's produce/consume split: `cache::Config::produce()`
yields a `cache::Producer` (the write half, not `Clone`), and `Producer::consume()` yields a
`cache::Consumer` (the read half, `Clone`). Names below are the real ones.

## Principles

These come from design review and pin down the shape:

- **Local, not on the wire.** The cache is local policy set by whoever holds a track endpoint
  (the relay or edge), never by the original publisher and never carried on the wire. This is
  why `TrackInfo.cache` goes away (see "Removing TrackInfo.cache"). The handle is **shareable**:
  one cache can back both a track's `TrackProducer` and its `TrackConsumer` (see "Attaching to a
  producer or a consumer").
- **Per-track bounds, no shared LRU.** Each track keeps a `[min, max]` window of its own recent
  groups. There is no cross-track accounting, so no shared lock and no contention. The cost is
  that there is no global RAM ceiling; total footprint is the sum of per-track `max` across
  live tracks. A global backstop, if ever needed, is additive and not part of v1.
- **No traits, no callbacks.** The cache is concrete values you configure and attach (`cache::Producer` / `cache::Consumer`). moq-net
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
// module moq_net::cache

/// Local cache policy for a single track. Not on the wire, not in TrackInfo.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
    pub ram: Bounds,                // keep >= min in RAM; flush the band once > max
    // forthcoming: disk + remote tiers (object_store, feature-gated) and an interval backstop.
}

/// A low/high watermark. The gap (max - min) is the flush batch size.
pub struct Bounds { pub min: Limit, pub max: Limit }

/// A bound expressed as a duration, a byte count, or both (first to trip wins).
/// All-None means unbounded as a high watermark, floor-zero as a low watermark.
pub struct Limit { pub duration: Option<Duration>, pub bytes: Option<u64> }
```

The implemented `cache::Config` has only `ram` so far (it is `#[non_exhaustive]`, so adding
`disk` / `remote` / `interval` later is additive). The forthcoming tier shape:

```rust
// forthcoming
pub struct Tier { pub store: /* path or url */ (), pub bounds: Bounds }
// Config gains: disk: Option<Tier>, remote: Option<Tier>, interval: Option<Duration>
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
    ram: BTreeMap<u64, cache::Group>,  // recent groups, keyed by sequence
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

## Attaching to a producer or a consumer

The cache splits into a write half and a read half, like the rest of moq-net. `cache::Producer`
fills the cache and is **not `Clone`** (a single writer); `cache::Consumer` is `Clone` and shares
the same store. `Producer::consume()` derives a reader, so **one cache backs both a track's
producer and its consumer**.

```rust
// implemented (RAM tier) in moq_net::cache
let writer: cache::Producer = config.produce();   // not Clone
let reader: cache::Consumer = writer.consume();    // Clone; shares the store

writer.insert(group);            // -> Option<Batch> (band to persist to the next tier)
reader.get(sequence);            // -> Option<cache::Group>
```

The forthcoming track wiring hands each endpoint the matching half:

```rust
// forthcoming
impl TrackProducer {
    /// Fill `cache` with groups this producer creates and serve them on a miss.
    pub fn with_cache(self, cache: cache::Producer) -> Self;
}
impl TrackConsumer {
    /// Back fetch_group / get_group with `cache`: hits resolve locally.
    pub fn with_cache(self, cache: cache::Consumer) -> Self;
}
```

Sharing one store across both endpoints of a track:

```rust
let writer = config.produce();
let reader = writer.consume();
let producer = producer.with_cache(writer);   // fills the cache
let consumer = consumer.with_cache(reader);   // fetches from it, same groups
```

`cache::Producer` being non-`Clone` is also a deliberate step toward making `TrackProducer`
non-`Clone`: a single writer per track.

### Producer side
`TrackState.groups` (today's inline buffer, bounded by the now-removed `TrackInfo.cache`) is
backed by the cache: finished groups beyond `ram.min` move into the RAM tier, and a `get_group`
or `dynamic()` miss consults the cache (RAM, then disk, then remote) before `NotFound`.

### Consumer side (fetch vs populate)
Reading and populating are different halves, which is what the produce/consume split buys:

- A `TrackConsumer` given a `cache::Consumer` (read half) checks the cache first on
  `fetch_group(seq)` / `get_group(seq)`: a **hit** resolves the `kio::Pending` locally with no
  wire FETCH (RAM synchronously, disk/remote after the ranged read); a **miss** falls through to
  the wire.
- To *populate* the cache (insert groups read off the wire or off a live `subscribe`), a consumer
  takes a `cache::Producer` (write half) instead. This is the archive's record-and-serve path: a
  cache-backed consumer with no live upstream fills tiers as it reads and answers FETCH straight
  from them.

A shared cache makes the two directions symmetric: groups a producer creates are fetchable
through a consumer of the same track, and groups a consumer pulled off the wire are servable by
the producer. Inserts dedup by sequence, so attaching one cache to both sides is safe.

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

- **moq-cli:** no cache, or a small RAM-only `cache::Config` for a single track. See "moq-cli
  flags" below for the concrete surface.
- **moq-relay:** one `cache::Config` template applied to every track it creates. Threading that
  config onto the tracks moq-net auto-creates during fan-out is the Origin follow-up; here it is
  just `TrackProducer::with_cache(writer)`. A relay RAM cache that spills to disk or S3 becomes
  configuration, not code.
- **moq-edge:** the same, plus its own dynamic-handler business logic on top.

## moq-cli flags

The cache is most useful on the commands that run a local origin and serve a broadcast back
(`moq serve`, `moq accept`), so a flattened `CacheArgs` group lands on those. The flags map onto
the `[min, max]` bounds and the tier cascade; an absent `--cache-ram` means no cache (today's
behavior). This is the proposed surface; wiring it waits on the track-endpoint `with_cache` API.

```rust
/// Retain recent groups so late subscribers and FETCHes get old content.
/// Absent `--cache-ram` leaves caching off.
#[derive(clap::Args, Clone, Default)]
pub struct CacheArgs {
    /// Keep up to this much of each track's recent groups in RAM (high watermark).
    /// Setting it enables the cache. e.g. `30s`.
    #[arg(long, value_parser = humantime::parse_duration)]
    pub cache_ram: Option<Duration>,

    /// RAM low watermark; a flush drains down to this, and the band between the two
    /// becomes one segment. Defaults to two-thirds of `--cache-ram`.
    #[arg(long, value_parser = humantime::parse_duration)]
    pub cache_ram_min: Option<Duration>,

    /// Also retain on local disk at this path (spill from RAM).
    #[arg(long)]
    pub cache_disk: Option<PathBuf>,

    /// How long to keep groups on disk before rolling up to remote (or dropping if
    /// no remote tier). e.g. `5m`.
    #[arg(long, value_parser = humantime::parse_duration)]
    pub cache_disk_age: Option<Duration>,

    /// Also retain in remote object storage, e.g. `s3://bucket/prefix`.
    #[arg(long)]
    pub cache_remote: Option<Url>,

    /// How long to keep groups in remote storage. Omit to keep forever.
    #[arg(long, value_parser = humantime::parse_duration)]
    pub cache_remote_age: Option<Duration>,

    /// Flush a partial RAM band after this long even below the high watermark, so a
    /// low data-rate track still spills. Mostly redundant with a duration `--cache-ram`.
    #[arg(long, value_parser = humantime::parse_duration)]
    pub cache_interval: Option<Duration>,
}
```

`CacheArgs` flattens into `Serve` and `Accept` (the relay-running commands), e.g.

```text
moq serve --broadcast bbb --cache-ram 30s --cache-disk /var/cache/moq --cache-disk-age 5m \
          --cache-remote s3://moq-archive/bbb --cache-remote-age 30d  fmp4 < bbb.mp4
```

and converts to a `cache::Config` whose halves go to each endpoint:

```rust
impl CacheArgs {
    /// None when `--cache-ram` is unset (caching disabled).
    pub fn config(&self) -> Option<moq_net::cache::Config> { /* flags -> bounds (+ tiers) */ }
}

// in run_serve / run_accept: produce the writer once, derive a reader, hand one to each endpoint.
if let Some(config) = args.config() {
    let writer = config.produce();
    let reader = writer.consume();   // same store; serves fetch_group locally
    producer = producer.with_cache(writer);
    // a TrackConsumer of the same track takes `reader`.
}
```

Notes: byte-budget variants (`--cache-ram-bytes`, etc.) are additive later; duration bounds
cover the common case. moq-cli parses straight from clap (no TOML merge), so plain
`Option<Duration>` is fine here. The relay (`rs/moq-relay`), which does merge TOML, would carry
the same flags under its `Option<T>` clobber rule.

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
