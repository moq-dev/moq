# Usage stats in the model

Status: proposed. Supersedes the `with_meter` / `set_meter` approach currently on
the `stats-in-model` branch (PR #1873).

## Why

We want every transport that drives the model types (moq-lite, IETF, and the
non-MoQ gateways: moq-srt, moq-rtc, moq-rtmp) to record per-broadcast usage
uniformly, without each one re-instrumenting its data path. The current branch
gets there, but the *attachment* mechanism is awkward:

- `Broadcast{Producer,Consumer}::with_meter(sink)` is a setter the relay/gateway
  calls after constructing or obtaining a broadcast. It exists only because the
  sink isn't known at construction.
- On the relay subscriber side the payload sink is pulled from a *separate*,
  non-bumping `subscriber_meter()` while the lifecycle guard (which counts the
  subscriber) lives elsewhere in a `stats_guards` map. Two handles for one
  broadcast is fragile and, as noted in review, can mask a "we forgot to count
  this" bug.
- There's no model-level notion of "how many viewers/publishers are live"; the
  stats layer reconstructs it with a per-session `SessionBroadcasts` sentinel.

## The idea

Put the usage sinks **in `BroadcastInfo`**, set them **at construction**, and let
the immutable `Arc<BroadcastInfo>` carry them down to every track, group, and
frame. `Usage` is atomics, so the model bumps through a shared `&Arc<Usage>` —
there is no mutation, so there is no setter (`with_meter`) and no `Arc::make_mut`.

```rust
// usage.rs
pub struct Usage {
    // payload, bumped as media flows
    groups:  AtomicU64,
    frames:  AtomicU64,
    bytes:   AtomicU64,
    // lifecycle, bumped as broadcast handles open/close (live = opened - closed)
    opened:  AtomicU64,
    closed:  AtomicU64,
}

// One per direction. Lives in BroadcastInfo, pub so a stats layer can read it.
pub struct BroadcastStats {
    pub producer: Arc<Usage>, // ingress
    pub consumer: Arc<Usage>, // egress
}

// broadcast.rs — now immutable after construction.
pub struct BroadcastInfo {
    pub hops:  OriginList,
    pub epoch: SystemTime,
    pub stats: BroadcastStats,
}
```

`BroadcastProducer`/`Consumer`/`Track*`/`Group*` stop carrying a bare
`meter: Arc<Usage>` and instead carry `broadcast: Arc<BroadcastInfo>`. A
producer-side type bumps `broadcast.stats.producer`; a consumer-side type bumps
`broadcast.stats.consumer`. The side is implied by the type, so no flags. `Group`
also gets an `Arc<TrackInfo>` so it reads `timescale` (and `cache`/`compress`)
from there instead of having `timescale` threaded separately.

## Attribution happens at construction, not via a setter

The sink for each side is known by whoever has the session context, at the moment
the broadcast handle comes into being:

- **Ingress (producer).** The publisher (relay subscriber loop, or a gateway)
  builds the broadcast with its ingress sink already in place:

  ```rust
  let stats = BroadcastStats { producer: ingress, ..Default::default() };
  let broadcast = BroadcastInfo { hops, epoch, stats }.produce();
  ```

  Every track it `create_track`s, and every group/frame under them, inherits
  `Arc<BroadcastInfo>` and bumps `stats.producer`. No `with_meter`.

- **Egress (consumer).** A consuming session never constructs the broadcast — it
  gets a `BroadcastConsumer` from the origin. So the **origin** attributes egress:
  the per-session `OriginConsumer` carries a stats context, and when it yields a
  `BroadcastConsumer` (`request_broadcast` / `announced`) it builds that
  consumer's own `Arc<BroadcastInfo>` — same `hops`/`epoch`, but
  `stats.consumer = this session's egress sink`. The producer's
  `Arc<BroadcastInfo>` is untouched; each consuming session gets its own, so
  **per-tier egress survives** (an External and an Internal viewer of the same
  broadcast bump different sinks) with zero mutation.

`BroadcastConsumer` is `{ state (shared channel), info: Arc<BroadcastInfo> }`;
only the `info` differs per session, and the origin sets it once when handing the
consumer out. `with_meter` disappears from every handler and gateway; at most it
survives as a private origin-internal helper.

How the origin gets the egress sink without the model depending on the stats
layer: `OriginConsumer` holds an `Option<Arc<dyn Fn(&Path) -> BroadcastStats>>`
(or a small trait object) supplied by the relay when it scopes the origin per
session. Default `None` => no-op sinks. The stats layer stays the only thing that
knows about tiers.

## Live viewer / publisher counts in the model

The model owns "is this broadcast being watched / published," replacing
`SessionBroadcasts`:

- A `BroadcastConsumer` counts as **one live viewer while it has ≥ 1 outstanding
  `TrackConsumer`**. It holds a shared refcount; each `TrackConsumer` from
  `track()` holds a drop-guard. `0 -> 1` bumps `stats.consumer.opened`, the last
  drop (`1 -> 0`) bumps `stats.consumer.closed`. Clones of one `BroadcastConsumer`
  share the refcount (one logical viewer).
- Symmetric on the producer side: a `BroadcastProducer` with ≥ 1 live track is one
  live publisher, bumping `stats.producer.opened`/`closed`.

Because the count atomics ride the per-session `Arc<BroadcastInfo>` (egress) /
per-publisher one (ingress), they land in the right tier's sink automatically. The
stats publish loop reads `opened - closed` as the live count and maps it onto the
existing `broadcasts` / `broadcasts_closed` fields, so the **published schema is
unchanged**.

## What this touches

- **`usage.rs`**: extend `Usage` with `opened`/`closed`; add `BroadcastStats`.
- **`broadcast.rs`**: `BroadcastInfo` gains `pub stats`; `Broadcast{Producer,
  Consumer}` carry `Arc<BroadcastInfo>`; `consume()` and the origin build the
  consumer's info; refcount + lifecycle bumps; delete `with_meter`.
- **`track.rs` / `group.rs`**: carry `Arc<BroadcastInfo>` (+ `Arc<TrackInfo>` to
  the group); bump the side-appropriate half; `GroupProducer::new` takes
  `Arc<TrackInfo>` instead of a bare `timescale`. `TrackProducer::new(name, info)`
  keeps its signature (standalone tracks get a default no-op `Arc<BroadcastInfo>`),
  so the ~80 deferred test sites are untouched.
- **`origin.rs`**: `OriginConsumer` carries the egress-sink provider and stamps it
  onto each `BroadcastConsumer` it yields.
- **`stats.rs`**: the per-broadcast handle hands out the `BroadcastStats` (the
  `Arc<Usage>` pair) instead of vending `with_meter`-style guards; the publish
  loop reads them; `SessionBroadcasts` goes away (model owns the counts). The
  current `BroadcastStats` type here is renamed `BroadcastHandle` to free the
  name.
- **handlers / gateways**: stop calling `with_meter`. Ingress builds the broadcast
  with its sink; egress just uses a stats-aware origin.

## Out of scope (deliberately)

- **The `Tier` (internal/external) split.** It's kept as-is here. The intent
  (separate billable customer traffic from cluster-internal forwarding) is real,
  but encoding it as a fixed binary enum threaded through the counter matrix is
  rigid; "internal" is really just "traffic under the cluster's auth root." Moving
  attribution to per-auth-root keying (and letting the downstream aggregator label
  roots as internal/external) is a cleaner model, but it reshapes the *published*
  stats format and deserves its own change.
- The published stats **schema** is unchanged: the model just drives the same
  fields from a cleaner place.

## Phasing

1. `usage.rs` + `BroadcastInfo` carry stats; thread `Arc<BroadcastInfo>` +
   `Arc<TrackInfo>` through the model; bumps move to `stats.{producer,consumer}`.
   Pure internal refactor, behavior-preserving. (Big diff: `GroupProducer::new`
   signature + the model tests that build groups directly.)
2. Origin attributes egress; ingress baked at construction; delete `with_meter`
   from handlers/gateways.
3. Model-tracked live viewer/publisher counts; drop `SessionBroadcasts`.
