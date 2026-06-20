# moq-archive (design)

> Status: design proposal, no implementation yet. Targets the `dev` branch because it
> builds on the moq-lite-05 FETCH API and media timestamps, both of which only exist there.

`moq-archive` saves a live MoQ track to durable storage and serves it back on demand. It
sits beside `moq-relay` in the stack: the relay keeps a small in-memory cache for live
fan-out, while the archive is the long-term tier that answers FETCH requests for groups
that have long since aged out of the relay's cache.

## Goals

- Record a single `TrackConsumer` to disk and/or object storage, losslessly, frames intact.
- Serve any previously recorded group back through the normal FETCH path, so an unmodified
  consumer can re-request old groups without knowing storage exists.
- Tier data across RAM, local disk, and remote object storage (S3/GCS/Azure), with an
  independent, optional retention duration per tier.
- Survive out-of-order group delivery (groups arrive on independent QUIC streams and can
  complete in any order).
- Avoid one-file-per-group: audio makes a group per frame, so a naive layout would create
  thousands of tiny files. Groups are batched into larger segment objects.

## Non-goals (v1)

- Whole-broadcast archival. There is currently no generic way to enumerate every track in a
  broadcast, so v1 records one track at a time. A broadcast-level wrapper (one recorder per
  track, shared root) is a follow-up once track discovery exists.
- Transcoding, repackaging, or media awareness. The archive treats frames as opaque sized
  payloads plus an optional timestamp, exactly like the relay. The catalog, container, and
  codec layers stay in `hang`.
- Live VOD playback / DVR scrubbing UX. v1 serves groups via FETCH; building a seekable
  player on top is downstream work.

## Prior art

There is no existing crate for this. A crates.io scan turns up only the `moq-dev` crates
(`moq-net`, `moq-relay`, `moq-mux`, ...), none of which persist tracks; the relay cache is
deliberately RAM-only and ephemeral. The IETF MoQ drafts leave durable storage to the
application. So we build it, but we do **not** hand-roll the storage backend:
[`object_store`](https://crates.io/crates/object_store) (0.13, Apache Arrow project) already
provides one trait over AWS S3, GCS, Azure Blob, local filesystem, in-memory, and HTTP, with
first-class byte-range GETs (`get_opts` / `GetRange`) and multipart uploads. That is exactly
the disk + S3 abstraction we need, so both the "disk" and the "S3" tiers are just two
`Arc<dyn ObjectStore>` instances and serving a group is one ranged GET.

### Approaches we borrow from

"Many small finite append-only streams, batched into larger objects with an index, tiered to
object storage" is a well-trodden pattern. We are reassembling established techniques, not
inventing one:

- **Apache BookKeeper "ledgers"** (Pulsar) are the closest match: a ledger is a finite,
  append-only, sealed sequence of entries, exactly a MoQ group; Pulsar offloads sealed ledgers
  to S3. A track is a chain of ledgers.
- **Kafka log segments + tiered storage (KIP-405)** already solved "don't make a file per
  record": batch records into large segments, each with an offset index, and offload old
  segments to S3. Our segment + per-group offset table is the same shape; the lesson stolen is
  a *sparse* index for very large archives (open question 5).
- **Facebook Haystack / f4** and **Bitcask** are the small-object fix: pack many small blobs
  into few large files with an in-memory `key -> (file, offset, len)` index. Our in-RAM
  `BTreeMap` is a Bitcask keydir.
- **Parquet footer + Iceberg manifest** shape our index: a self-describing per-segment footer
  plus a per-track manifest that routes a sequence to a segment.

No single embeddable crate does all of (batch + index + S3 tiering) for opaque streams: the
systems that do (BookKeeper, Kafka, Pravega) are servers, not libraries. An embedded LSM
(`fjall`, `redb`, or RocksDB BlobDB) would give batching, an index, and compaction for free but
is local-disk only with **no S3 tier**, which is the whole point here. So `object_store` for
the tiers plus our own thin segment/manifest format is the smallest thing that fits; an
embedded LSM as the *disk-tier engine* remains a viable future swap behind the `Storage` trait.

## Background: the moq-net model we build on

The relevant `moq-net` (dev) API, recapped so the design is self-contained:

**Recording side (read a track):**
- `TrackConsumer::subscribe(None) -> TrackSubscriber`.
- `TrackSubscriber::recv_group() -> Result<Option<GroupConsumer>>` yields every group in
  *arrival* order (preserves out-of-order delivery; `next_group()` would skip late arrivals,
  which we do not want for an archive).
- `GroupConsumer { sequence }`, `GroupConsumer::read_frame() -> Result<Option<Bytes>>` drains
  frames in order; `Frame { size, timestamp: Option<Timestamp> }` carries the media timestamp
  when present (moq-lite-05+).
- `GroupConsumer::finished() -> Result<u64>` resolves once the group is complete, returning
  the final frame count. This is our "safe to flush" signal.

**Serving side (answer fetches):**
- `TrackProducer::dynamic() -> TrackDynamic`.
- `TrackDynamic::requested_group() -> Result<GroupRequest>` blocks until a consumer FETCHes a
  group that is not in the producer's cache. While any `TrackDynamic` handle is alive, the
  miss waits to be served instead of failing fast with `NotFound`.
- `GroupRequest::sequence() -> u64`, `GroupRequest::accept(info) -> GroupProducer`. We fill the
  returned `GroupProducer` with `create_frame` / `write` / `finish` from storage, then
  `GroupProducer::finish()`.

The archive's serving entry point is therefore a **`TrackDynamic`, not a `TrackProducer`**:
the caller owns the `TrackProducer` (and publishing the broadcast into a session via
`BroadcastProducer` / `OriginProducer::publish_broadcast`), calls `.dynamic()`, and hands the
archive the request side. This makes the archive one composable link in a fallback chain
rather than a thing that owns the track.

**The archive is a link in a cache chain.** The caller decides where it sits. For example
`moq-relay` would try the archive first for any dynamic request, and the archive answers from
RAM/disk/S3. On a miss, the request must fall through to the **original publisher** (the live
origin), because a group might never have reached storage. So the archive needs both an
incoming `TrackDynamic` (requests from downstream) *and* a downstream handle to forward misses
to (its own `TrackDynamic` over the upstream track, which it also records). The chain is:

```
consumer FETCH -> relay cache -> moq-archive (RAM -> disk -> S3) -> origin publisher
```

> **Open architectural question (from review):** this cache-fallback-plus-record behavior
> could instead live *inside* `moq_net::TrackProducer` / `TrackConsumer` themselves. That is a
> friendlier API (the archive becomes a storage backend you attach, not a chain you wire) and
> moq-net would then know precisely when a group is evicted from its RAM cache, which is the
> natural trigger to flush. The cost is baking storage concerns into the core wire types, which
> feels out of place there. v1 keeps the logic in `moq-archive` and treats moq-net integration
> as a follow-up; flagged here because it shapes the public API.

The two directions stay decoupled: recording needs a `TrackConsumer`, serving needs a
`TrackDynamic` (plus an upstream handle for miss fallback). They share only the storage layer,
so an archive node can do either or both.

## Architecture

```
              record (TrackConsumer)                      serve (TrackProducer + TrackDynamic)
                      |                                                   ^
                      v                                                   |
   +------------------------------------+                  +------------------------------+
   |  Writer                            |                  |  Reader                      |
   |  - drain groups in arrival order   |                  |  - on GroupRequest(seq):     |
   |  - buffer in RAM until finished()  |                  |    look up seq in Index      |
   |  - batch completed groups          |                  |    ranged GET the segment    |
   |  - flush segment + index entries   |                  |    parse frames, stream out  |
   +------------------------------------+                  +------------------------------+
                      |                                                   ^
                      v                                                   |
   +-------------------------------------------------------------------------------------+
   |  Storage (tiered)                                                                    |
   |    RAM ring  --(flush)-->  disk store  --(age + aggregate)-->  S3 store              |
   |    Index: group seq -> (tier, object key, byte offset, length, frame count, ts span) |
   +-------------------------------------------------------------------------------------+
```

Two halves, joined by a `Storage` abstraction and an `Index`:

### Writer (ingest)

1. Subscribe to the source `TrackConsumer` and loop on `recv_group()`.
2. For each group, spawn/track a buffer that drains `read_frame()` into an in-RAM
   `BufferedGroup { sequence, frames: Vec<(Option<Timestamp>, Bytes)> }`. Because groups
   arrive concurrently, several buffers are open at once, keyed by sequence.
3. When a group's `finished()` resolves, mark it flushable. Incomplete groups never leave RAM
   (we cannot serve a half-group).
4. A flusher batches flushable groups into one **segment object** (footer included) when a
   threshold trips: a byte-size target, the RAM time window, or a group going `unused()` early.
   Batching is what keeps audio from making a file per frame. The latest group and any `used()`
   groups stay in RAM.
5. Tier maintenance runs on a timer: roll up (concatenate several disk segments into one larger
   S3 object, rewriting the manifest), then LRU/age-evict and delete objects past each tier's
   budget.

### Reader (egress / serve)

1. Take the incoming `TrackDynamic` (the caller owns the `TrackProducer` and publishes it).
   Optionally hold a downstream handle to the upstream origin for miss fallback.
2. Loop on `TrackDynamic::requested_group()`. For each `GroupRequest(seq)`:
   - Look up `seq` in the `Index` to find `(store, object key, offset, length)`.
   - `store.get_opts(key, GetRange::Bounded(offset..offset+length))` -> the segment slice for
     that one group (a single range request, S3-friendly).
   - Parse the group's frames, `request.accept()`, and stream them into the `GroupProducer`.
   - On a miss (not in RAM/disk/S3), forward the request to the upstream origin if present,
     relaying (and recording) the result. Only when nothing upstream can satisfy it does the
     request resolve to `NotFound`.
   - On a miss (seq never recorded or already evicted) reject with `Error::NotFound`.

Reader and Writer are independent tasks sharing `Storage`; an archive process can run one or
both. The in-RAM tier doubles as a serving cache: a FETCH for a still-buffered recent group is
served from memory without touching disk.

## Storage layout

Everything is an `object_store` key, so the same code paths work for a local dir
(`LocalFileSystem`) and a bucket (`AmazonS3`). Proposed key scheme, rooted at a configurable
prefix and namespaced by broadcast/track:

```
<root>/<broadcast>/<track>/segments/<segment-id>      # concatenated groups
<root>/<broadcast>/<track>/manifest                   # append-only list of segments (see Index format)
```

**Broadcast and track names contain slashes** (they are themselves path-shaped, e.g.
`room/alice/camera`). `object_store` paths are `/`-delimited, so the raw name would explode
into spurious directory levels and collide (`a/b` + `c` vs `a` + `b/c`). Percent-encode each
name as a single, reversible path segment before use (encode `/` and any other delimiter), so
`<broadcast>` / `<track>` are opaque components. This keeps `list`-by-prefix working per
broadcast and lets us recover the original names on restart.

### Segment format

A segment is a concatenation of groups. Each group is self-delimiting so a ranged GET of just
its slice is independently parseable:

```
group   := group_header frame*
group_header := varint(sequence) varint(frame_count)
frame   := varint(size) flags ts? payload[size]
            flags: 1 byte; bit0 = timestamp present
            ts:    varint(zigzag delta vs previous frame ts in this group)   # when bit0 set
```

This mirrors moq-net's own frame coding (size-prefixed, optional zigzag-delta timestamp) so
there is no information loss across a record/serve round-trip. The varint/zigzag helpers are
small; if moq-net's `coding` module is made `pub(crate)`-exportable we reuse it, otherwise a
~30-line local copy (the wire format is stable). Frame payloads are stored verbatim;
compression is a later option.

### Index format (nailed down)

Two levels, modeled on Parquet's self-describing footer plus an Iceberg-style manifest. This
avoids a separate `.idx` object per segment (which would reintroduce the small-object problem)
and makes each segment independently recoverable.

**1. Per-segment footer.** Each segment object ends with its own group table plus a fixed
trailer, so the segment is self-describing: given only the object you can find every group in
it. The table is one record per group:

```rust
struct GroupEntry {
    sequence: u64,           // group sequence (NOT necessarily contiguous or sorted)
    offset: u64,             // byte offset of the group within this segment
    length: u64,             // byte length of the group
    frames: u32,             // frame count (validates / sizes the GroupProducer)
    ts_first: Option<i64>,   // media timestamp span (retention + future seeking)
    ts_last: Option<i64>,
    received: u64,           // wall-clock ms at completion; retention fallback when ts absent
}

// segment := group* footer
// footer  := postcard(Vec<GroupEntry>) u32(footer_len) u32(magic)
```

**2. Per-track manifest.** One append-only object per track listing its segments, so the
reader can route a sequence to a segment without opening every segment footer:

```rust
struct ManifestEntry {
    segment: SegmentId,      // object key (relative to the track prefix)
    tier: Tier,              // Disk | S3 (RAM segments are not in the manifest)
    seq_min: u64, seq_max: u64,   // sequence range covered (groups out of order, so a range, not a set)
    ts_min: Option<i64>, ts_max: Option<i64>,
}
```

**Encoding:** `postcard` for both (compact, `serde`, no schema server; chosen over JSONL so
the footer is fixed-shape and the manifest stays small for multi-day archives). The trailer's
`footer_len` + `magic` let the reader fetch the footer with one tail range GET
(`GetRange::Suffix`) without knowing its size up front.

**Runtime + recovery:** on startup the reader reads each track manifest, building an in-RAM
`BTreeMap<u64, (SegmentId, GroupEntry)>` for O(log n) seq lookup (segment footers are fetched
and cached lazily on first hit). Because groups complete out of order, footer entries are in
completion order; the `BTreeMap` makes lookup order-independent. The manifest is the routing
index; segment footers are the source of truth and let us rebuild a manifest by `list` +
tail-read if one is ever lost.

## Tiering and retention

Three tiers, RAM -> disk -> S3, each optional. The key idea (from review): **each rollup step
merges multiple units from the tier above into one larger object, so fragmentation decreases
as data moves down.** RAM can be highly fragmented (one buffer per group, audio makes many);
disk segments coalesce a window of groups; S3 objects coalesce a window of disk segments.

| Tier | Backed by | Holds for | Flushes downward in |
|------|-----------|-----------|---------------------|
| RAM  | in-process buffers | up to e.g. 30s | 10s segments to disk |
| Disk | `object_store` `LocalFileSystem` | up to e.g. 5m | 1m batches to S3 |
| S3   | `object_store` `AmazonS3` (GCS/Azure) | up to e.g. 30d (or forever) | n/a (final tier) |

So a group is written many times to RAM individually, rewritten once into a 10s disk segment,
then several disk segments are concatenated into one 1m S3 object. This directly resolves the
old "1:1 copy vs concatenate" open question in favor of **concatenate at every rollup**.

### Eviction: LRU + size budget, not just age

Each tier has both a **max age** and a **size budget**. Within a tier, evict **least-recently-
used** first (an LRU keyed by group/segment), capped by the budget; the max age is an upper
bound layered on top. LRU is the right default for both RAM and disk because serving traffic
is bursty and skewed (a re-fetched group is likely to be re-fetched again). Pure-RAM mode (no
disk, no S3) is then a bounded LRU/DVR window rather than a strict ring buffer.

Two refinements from review:

- **Always keep the latest group in RAM**, exempt from LRU/age eviction. New subscribers and
  the live edge need it immediately, and it is the one group most likely to be requested next.
- **Use moq-net's `used()` / `unused()` group state to flush early.** A group that has gone
  `unused` (no active consumer interested) earns nothing by staying in RAM, so fold it into the
  next disk flush instead of waiting out the full RAM window. `used()` groups stay hot. This
  makes the RAM age a cap, not a fixed delay, and reclaims memory under churn.

### Retention clock

For the max-age bound, prefer the group's **media timestamp** (`ts_last`) when present
(moq-lite-05), else fall back to the **wall-clock `received` time**. A tier is *enabled* when
its store is configured; "S3 forever" is `s3.store = Some` with no max age. Data flows strictly
downward, so disabling the middle tier flushes RAM straight to S3. Segments/objects are deleted
whole once every group they contain has aged out, so the rollup batch granularity bounds how
long one live group pins an object.

All tiers live in a `Storage` struct shared by the writer, reader, and a single periodic
maintenance task that does the three jobs: roll up (merge + promote), LRU/age evict, delete.

## Public API sketch

Smallest surface that does the job, per the repo's public-API guidance. One insulated entry
point per direction, plus a `#[non_exhaustive]` config built via `Default`.

```rust
/// Per-tier sizing. Build via `Config::default()` then set fields.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
    pub ram: RamConfig,             // memory window + budget; always keeps the latest group
    pub disk: Option<TierConfig>,   // LocalFileSystem store
    pub s3: Option<TierConfig>,     // remote object_store; no max_age -> keep forever
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TierConfig {
    pub store: Arc<dyn object_store::ObjectStore>,
    pub prefix: object_store::path::Path,
    pub max_age: Option<Duration>,  // upper bound; None on S3 means forever
    pub budget: Option<u64>,        // byte budget, LRU-evicted when exceeded
    pub rollup: Duration,           // window of upstream units merged into one object here
}

/// An archive for a single track, over shared tiered storage.
pub struct Archive { /* Storage + Index */ }

impl Archive {
    pub fn new(config: Config) -> Result<Self>;

    /// Record a live track until it ends or errors. Drains groups, batches, flushes.
    pub async fn record(&self, track: TrackConsumer) -> Result<()>;

    /// Answer FETCH requests from storage. Takes the request side of a track the caller owns
    /// and publishes; on a storage miss, forwards to `upstream` (the live origin) if given,
    /// otherwise resolves the request to `NotFound`.
    pub async fn serve(
        &self,
        requests: TrackDynamic,
        upstream: impl Into<Option<TrackConsumer>>,
    ) -> Result<()>;
}
```

`serve` takes a `TrackDynamic`, not a `TrackProducer`: the caller owns the producer and
publishes the broadcast into a session, so the archive is a composable link in the cache chain
(relay -> archive -> origin) rather than the owner of the track. `upstream` is the miss-
fallback handle and, when recording the same track, the source the writer drains. Per the repo
convention it is `impl Into<Option<TrackConsumer>>`, so callers pass the consumer or `None`.

## Usage mockups

These compile-in-spirit sketches answer "how is this actually called?" and surface a real gap:
the per-track API above serves a **standalone / VOD** node cleanly, but `moq-relay` has no
seam to use it, which is the argument for the moq-net integration below.

### A. Standalone VOD node (works with the per-track API)

A node that recorded a broadcast earlier and now serves it back. Here the archive *is* the
publisher: the live broadcast is gone, so there is no path collision and the per-track API
fits. Recording is symmetric (`origin.consume()` -> `BroadcastConsumer::track` ->
`Archive::record`).

```rust
let archive = Archive::new(config)?;

// Publish a broadcast; its tracks answer FETCH from storage.
let broadcast = BroadcastInfo::new().produce();
origin.publish_broadcast("vod/room-alice", broadcast.consume())?;

// For each track a downstream subscriber asks for, serve it from storage.
let mut tracks = broadcast.dynamic();
while let Ok(request) = tracks.requested_track().await {
    let producer = request.accept(TrackInfo::default())?;   // caller owns the producer
    let requests = producer.dynamic();                      // archive gets the request side
    tokio::spawn(archive.serve(requests, None));            // no upstream: pure VOD
}
```

### B. Why `moq-relay` cannot use the per-track API as-is

The relay is built entirely around a single `OriginProducer` (`Cluster::origin`). Remote
publishers `publish_broadcast` into it; downstream sessions read `origin.consume()`. The relay
code never constructs a `TrackProducer` and never calls `.dynamic()` on a track. That happens
*inside* moq-net's session fan-out (`lite::subscriber` / `ietf::subscriber`), which creates the
per-track producer and its `TrackDynamic` to forward a downstream cache-miss FETCH upstream.

So there is no point in `moq-relay` where you could write `archive.serve(track_dynamic, ...)`:
the relay operates one layer up, at the broadcast/origin granularity, and the track objects the
archive needs only exist transiently deep inside moq-net. Wiring the per-track API into the
relay would mean interposing on every track of every forwarded broadcast (republishing each
broadcast through an archive-owned `BroadcastProducer`, re-`accept`ing every `requested_track`,
re-`subscribe`ing upstream), i.e. reimplementing the relay's fan-out around the archive. That
is the "wire a fallback chain" cost, and it is large.

### C. The moq-net seam (what actually makes the relay one line)

Give moq-net a pluggable cache backend that it consults on a miss and notifies on eviction,
attached where it already owns the per-track RAM cache (the origin, flowing down to each
`TrackProducer`). The archive implements the trait; the relay attaches it once.

```rust
// moq-net (new): the one hook the relay can't get from outside is *when* a group is evicted.
pub trait Cache: Send + Sync + 'static {
    /// A group aged out of the RAM cache. Persist it (called with the finished group).
    fn store(&self, track: &TrackInfo, group: GroupConsumer);

    /// A consumer fetched a group not in RAM. Produce it from storage into `request`,
    /// or return it unserved so moq-net falls through to the upstream wire FETCH.
    async fn fetch(&self, track: &TrackInfo, request: GroupRequest);
}

impl moq_net::Cache for Archive { /* store -> writer, fetch -> reader */ }

// moq-relay: the entire integration.
let origin = Origin::random().produce().with_cache(archive);
```

Now the relay keeps working at the origin level, the per-track plumbing stays inside moq-net,
and the archive transparently catches evictions (the natural flush trigger) and serves misses
before they cost an upstream round-trip. This is the recommended shape; it makes the public
`Archive` a `Cache` impl plus the standalone `record`/`serve` helpers from scenario A, rather
than the chain wiring. Decision needed before the API is locked (see open question 1).

## Binary

`moq-archive` (the binary) wires the library to a relay, mirroring `moq-cli`:

- clap config, TOML-loadable. Every `#[arg]` field is `Option<T>` so the TOML->CLI merge does
  not clobber file values with `Default` (repo rule; add the regression test like
  `moq-relay`). Durations use `humantime-serde`.
- Subcommands: `record --url <relay> --broadcast <name> --track <name>` connects, subscribes,
  and records; `serve ...` connects, publishes, and answers fetches. A combined mode runs both.
- Storage flags map onto `object_store` builders: `--disk <path>`, `--s3-url s3://bucket/prefix`
  (+ standard AWS env for creds). Each tier takes a max age, a byte budget, and a rollup window,
  e.g. `--ram-age 30s --disk-age 5m --disk-rollup 10s --s3-age 30d --s3-rollup 1m`.

## Out-of-order handling (why it is first-class)

Groups ride independent QUIC streams, so sequence 7 can finish before sequence 5. The writer
therefore keeps a map of open buffers and only flushes a group on its own `finished()`; it
never assumes contiguity. The index is keyed by sequence but appended in completion order, and
the reader's `BTreeMap` makes lookup order-independent. FETCH is inherently random-access
(consumer asks for an arbitrary old seq), so the read path has no ordering assumptions either.
Sequence gaps (a group that was lost upstream and never recorded) are legal: a FETCH for a gap
returns `NotFound`.

## Open questions

1. **moq-net integration (the big one).** Should the cache-fallback-plus-record behavior live
   inside `moq_net::TrackProducer` / `TrackConsumer` instead of being wired as a chain by the
   caller? Friendlier API and moq-net would know exactly when a group leaves its RAM cache (the
   natural flush trigger), at the cost of putting storage concerns in the core wire types. See
   the architecture section. Decide before locking the public API.
2. **Restart/recovery.** Rebuild the in-RAM `BTreeMap` from each track manifest on startup;
   refetch segment footers lazily. Crash-consistency: write the segment object (footer last)
   before appending to the manifest, so a half-written segment is simply unreferenced and a
   startup `list` sweep GCs any segment missing from the manifest.
3. **Sub-group FETCH.** Earlier drafts let a FETCH start at frame K within a group. This is
   likely being **removed from moq-lite-05** and the current API does not support it, so the
   archive serves whole groups only. If it returns, the cheap path (ranged-GET the group, skip
   K frames in memory; groups are bounded at 32 MB) suffices before adding per-frame offsets.
4. **Serving the live edge.** Keeping the latest group in RAM plus upstream miss-fallback lets
   the archive answer recent FETCHes and stand in for a departed origin. A full live
   `subscribe` replay (DVR-style) is a possible follow-up beyond FETCH.
5. **Very large archives.** The manifest + lazily-cached footers handle a multi-day archive,
   but a months-long one may want manifest sharding (per time bucket) so startup does not read
   the whole thing. Defer until the single-manifest form is measured.
6. **Backpressure.** If storage is slower than ingest, do we drop oldest buffered groups
   (bounded memory, lossy) or apply backpressure to the subscription? Recommend a bounded RAM
   budget that LRU-drops oldest *completed-but-unflushed* groups and records the gap, never
   blocking live ingest.

## Testing plan

- Unit: segment encode/decode round-trip, including absent vs present timestamps and
  zigzag-delta edges; index append + reload; out-of-order completion ordering.
- Storage: run the full record -> flush -> serve loop against `object_store`'s in-memory and
  `LocalFileSystem` backends (no network needed). Use `tokio::time::pause()` for retention/tier
  timers per the repo's async-test rule.
- Integration: record a synthetic track (audio-shaped: one frame per group), serve it back via
  FETCH, assert byte-exact frames and timestamps. Confirm a FETCH for an evicted/gap sequence
  returns `NotFound`.
- Config: TOML<->CLI merge regression test (the `Option<T>` flag rule).

## Cross-package sync

Per the repo's sync table, a new standalone crate that only *consumes* `moq-net`'s public API
needs no wire/catalog changes. Touch points:

- Add `rs/moq-archive` to the workspace `members` / `default-members` in the root `Cargo.toml`.
- New docs page under `doc/bin/` for the binary (and a `doc/concept/` note on the archive tier
  relative to the relay cache).
- If we end up needing moq-net's `coding` varint/zigzag helpers, that is a small additive
  `pub` export in `moq-net` (non-breaking), to avoid duplicating the wire codec.
```
