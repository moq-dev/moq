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

**Serving side (produce a track + answer fetches):**
- `TrackProducer::dynamic() -> TrackDynamic`.
- `TrackDynamic::requested_group() -> Result<GroupRequest>` blocks until a consumer FETCHes a
  group that is not in the producer's cache. While any `TrackDynamic` handle is alive, the
  miss waits to be served instead of failing fast with `NotFound`.
- `GroupRequest::sequence() -> u64`, `GroupRequest::accept(info) -> GroupProducer`. We fill the
  returned `GroupProducer` with `create_frame` / `write` / `finish` from storage, then
  `GroupProducer::finish()`.
- To expose the track over a session, wrap the producer in a `BroadcastProducer` and publish
  via `OriginProducer::publish_broadcast`, then connect with `moq-native` (same as `moq-cli`).

The two directions are deliberately decoupled: recording needs a `TrackConsumer`, serving
needs a `TrackProducer` + `TrackDynamic`. They share only the storage layer, so an archive
node can do either or both.

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
4. A flusher batches flushable groups and writes them as one **segment object** plus appended
   **index entries** when either threshold trips: a byte-size target (e.g. a few MB) or a time
   window (the RAM retention duration). Batching is what keeps audio from making a file per
   frame.
5. Tier maintenance runs on a timer: promote aged segments disk -> S3 (optionally aggregating
   several small disk segments into one larger S3 object), and delete objects past each tier's
   retention.

### Reader (egress / serve)

1. Hold a `TrackProducer` for the recorded track plus a `TrackDynamic`. Publish the broadcast
   into an origin/session so consumers can reach it.
2. Loop on `TrackDynamic::requested_group()`. For each `GroupRequest(seq)`:
   - Look up `seq` in the `Index` to find `(store, object key, offset, length)`.
   - `store.get_opts(key, GetRange::Bounded(offset..offset+length))` -> the segment slice for
     that one group (a single range request, S3-friendly).
   - Parse the group's frames, `request.accept()`, and stream them into the `GroupProducer`
     (honoring `frame_start` by skipping the first N frames; see Open questions).
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
<root>/<broadcast>/<track>/index/<segment-id>.idx     # entries for that segment (or one rolling index)
```

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

### Index format

The index maps group sequence to its physical location. One entry per group:

```rust
struct IndexEntry {
    sequence: u64,           // group sequence (NOT necessarily contiguous or sorted)
    segment: SegmentId,      // which segment object
    offset: u64,             // byte offset of the group within the segment
    length: u64,             // byte length of the group
    frames: u32,             // frame count (lets us validate / size the GroupProducer)
    ts_first: Option<i64>,   // media timestamp span, used for retention + future seeking
    ts_last: Option<i64>,
    received: u64,           // wall-clock ms at completion, retention fallback when ts absent
}
```

Because groups complete out of order, entries are appended in completion order, not sequence
order. The reader loads them into a `BTreeMap<u64, IndexEntry>` (or per-segment index files
merged on startup) for O(log n) seq lookup. For v1 the index is JSON Lines: append-friendly,
trivially debuggable, and small relative to media. If it grows hot we switch the on-disk form
to `postcard`/`bincode` behind the same `Index` type. The in-RAM `BTreeMap` is the source of
truth at runtime; index objects are how we rebuild it after a restart.

## Tiering and retention

Three tiers, each optional, each with an optional retention `Duration`:

| Tier | Backed by | `retain: Option<Duration>` meaning |
|------|-----------|-----------------------------------|
| RAM  | in-process buffers | how long *completed* groups linger in memory after flush (serving cache window). `None` -> drop right after flush. Incomplete groups always stay regardless. |
| Disk | `object_store` `LocalFileSystem` | how long segments stay on local disk before promotion to S3 (and deletion locally). `None` -> stay until evicted by the final-tier rule. |
| S3   | `object_store` `AmazonS3` (or GCS/Azure) | how long aggregated objects stay in the cloud. `None` -> keep forever. |

Rules:
- A tier is *enabled* when its store is configured. Durations only cap retention within an
  enabled tier; enabling/disabling a tier is separate from its duration (so "S3 forever" is
  `s3.store = Some, s3.retain = None`).
- Data flows strictly downward: RAM -> disk -> S3. Disabling the middle tier (no disk store)
  flushes RAM straight to S3.
- Pure RAM mode (no disk, no S3) is a bounded in-memory ring buffer, an ephemeral DVR window.
- Retention clock: prefer the group's **media timestamp** (`ts_last`) when present
  (moq-lite-05), else fall back to the **wall-clock `received` time**. Evict a group from a
  tier once `now - clock(group) > retain`, deleting the segment/index objects once every group
  they contain has aged out (segments are evicted whole, so the flush batch granularity bounds
  how long a single live group pins a segment).

All three live in a `Storage` struct so the writer, reader, and the maintenance timer share
one view. Tier maintenance is a single periodic task: promote, aggregate, delete.

## Public API sketch

Smallest surface that does the job, per the repo's public-API guidance. One insulated entry
point per direction, plus a `#[non_exhaustive]` config built via `Default`.

```rust
/// Where and how long to retain each tier. Build via `Config::default()` then set fields.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
    pub disk: Option<TierConfig>,   // LocalFileSystem store + retention
    pub s3: Option<TierConfig>,     // remote object_store + retention
    pub ram: Option<Duration>,      // completed-group memory window
    pub flush: FlushConfig,         // batch size + interval thresholds
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TierConfig {
    pub store: Arc<dyn object_store::ObjectStore>,
    pub prefix: object_store::path::Path,
    pub retain: Option<Duration>,
}

/// An archive for a single track, over shared tiered storage.
pub struct Archive { /* Storage + Index */ }

impl Archive {
    pub fn new(config: Config) -> Result<Self>;

    /// Record a live track until it ends or errors. Drains groups, batches, flushes.
    pub async fn record(&self, track: TrackConsumer) -> Result<()>;

    /// Serve recorded groups: answers `TrackDynamic` FETCH requests from storage.
    /// Pass the producer side of the track you publish into an origin.
    pub async fn serve(&self, track: TrackProducer) -> Result<()>;
}
```

`serve` takes the `TrackProducer` (it calls `.dynamic()` internally and owns the request
loop), which matches the user's instinct that the serving side is really about `TrackDynamic`.
The caller still owns publishing the broadcast into a session, keeping `moq-archive` free of
networking policy.

## Binary

`moq-archive` (the binary) wires the library to a relay, mirroring `moq-cli`:

- clap config, TOML-loadable. Every `#[arg]` field is `Option<T>` so the TOML->CLI merge does
  not clobber file values with `Default` (repo rule; add the regression test like
  `moq-relay`). Durations use `humantime-serde`.
- Subcommands: `record --url <relay> --broadcast <name> --track <name>` connects, subscribes,
  and records; `serve ...` connects, publishes, and answers fetches. A combined mode runs both.
- Storage flags map onto `object_store` builders: `--disk <path>`, `--s3-url s3://bucket/prefix`
  (+ standard AWS env for creds), `--ram 30s --disk 1h --s3 30d`.

## Out-of-order handling (why it is first-class)

Groups ride independent QUIC streams, so sequence 7 can finish before sequence 5. The writer
therefore keeps a map of open buffers and only flushes a group on its own `finished()`; it
never assumes contiguity. The index is keyed by sequence but appended in completion order, and
the reader's `BTreeMap` makes lookup order-independent. FETCH is inherently random-access
(consumer asks for an arbitrary old seq), so the read path has no ordering assumptions either.
Sequence gaps (a group that was lost upstream and never recorded) are legal: a FETCH for a gap
returns `NotFound`.

## Open questions

1. **`frame_start` granularity.** moq-lite-05 FETCH can request "group N starting at frame K".
   The cheap path: ranged-GET the whole group, parse, skip K frames in memory (groups are
   bounded at 32 MB, so this is fine). The optimization: store per-frame offsets in the index
   for a partial ranged GET. Recommend the cheap path for v1, add per-frame offsets only if
   profiling demands it.
2. **Restart/recovery.** Rebuild the `BTreeMap` by listing + reading index objects on startup.
   Need a crash-consistency story: write the segment object first, then its index entries, so a
   half-written segment is simply never indexed (and is GC'd by a startup sweep of unindexed
   segments).
3. **Aggregation/compaction shape.** When promoting disk -> S3, do we copy segments 1:1 or
   concatenate many small disk segments into one big S3 object (rewriting offsets in the
   index)? Concatenation is better for S3 request economics but adds a rewrite step. Lean
   toward 1:1 in v1, compaction later.
4. **Serving the *latest* group / live edge.** v1 answers FETCH for past groups. Should the
   archive also serve a live `subscribe` (replay newest groups as they land) so it can stand in
   for a departed origin? That is closer to DVR and probably a follow-up.
5. **Index for a hot, long archive.** A multi-day archive has a large index. JSONL + in-RAM
   `BTreeMap` is fine for v1; a segmented/columnar index (or sqlite) may be needed at scale.
6. **Backpressure.** If storage is slower than ingest, do we drop oldest buffered groups
   (bounded memory, lossy) or apply backpressure to the subscription? Recommend a bounded RAM
   budget that drops oldest *completed-but-unflushed* groups and records the gap, never blocking
   live ingest.

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
