# Relay memory per announcement

## Goal

A relay's memory scales with what it serves, not with what the mesh knows.
Every relay currently materializes a full broadcast object graph for every
announcement anywhere in the cluster, subscribed or not, once per neighbor that
advertises it.

Measured on `moq-net` (`adad52b`) with an RSS plus allocator-counting harness:
8.8 KB per announced broadcast with one route, plus 4.3 KB per additional
route, in ~30 allocations and 2 spawned tasks. The announcement itself (the
path string) is 18 B of that, and `PathOwned` already shares one `Arc<str>`
across every copy, so the wire object is not the problem.

## Plan

### Root cause

`kio::State<T>` carries three `WaiterList`s, and each is a
`SmallVec<[Weak<Waker>; 32]>`: 280 B of inline waker slots paid unconditionally
inside the `Arc<Mutex<...>>`, so `size_of::<State<()>>()` is 848 B before `T`.
A broadcast holds four to five of those cells (the source's `Shared` and
`Alive`, the front's `Shared` and `Alive`, and `FrontState`), so ~4.2 KB of the
8.8 KB is empty slots. [Waiter slots](/quest/m2/relay-memory/waiters.md) is that struct.

The per-route half is closed. It used to be that every received announcement ran
the whole `create_broadcast` path, minting its own `broadcast::Producer` and
lifecycle task even for a standby route that was never selected, so a relay of
degree `d` attached `d` sources to one front and degree was a straight
multiplier. [moq#3225](https://github.com/moq-dev/moq/pull/3225) removed that:
`lite::subscriber` now calls `origin::Producer::announce_served`, so an
announcement costs a `RouteEntry` plus one `ServeState` whose `served` cache
materializes a broadcast only when something requests a path. That is what the
standby-routes quest asked for, delivered as a side effect, and the quest is
closed.

A group holds one of the same cells, which is why [group
charge](/quest/m0/group-charge.md) is an accounting bug and not only a footprint one.

### What it costs today

Per broadcast at degree `d` a relay pays `8.8 KB + (d - 1) * 4.3 KB` of origin
state, plus about `d * 0.5 KB` of per-peer session bookkeeping: every peer we
advertise the path to holds an `announce_ids` entry (32 B), a `held` entry
(144 B) and a `watched` entry (248 B), plus hashbrown's load factor. That
second term is summed from `size_of` rather than measured end to end, unlike
the origin figures, so treat it as good to a KB rather than exact. On a 1 GB
node at degree 5 (moq.pro's downstream APAC hub is exactly that, with half of
RAM given to the group cache and health shedding at 75%), announce state has
roughly 200 MB of headroom against a warm cache, so the node sheds itself out
of serving near 7,000 concurrent cluster-wide broadcasts.

Every figure above predates prefix routes and is no longer trustworthy: they
were measured on `adad52b`, where an announcement built a broadcast. A
`ServeState` still carries kio state cells, so the waiter-slot lever below still
applies to it, but the per-route multiplier those numbers describe is gone.
Remeasure before treating any of them as current.

### Why now rather than later

Nothing is near the ceiling today. Two committed directions move it:

- Chat-shaped traffic (a moq.pro downstream direction) names announcement
  churn as the control-plane risk. One broadcast per channel is ~10k
  (~270 MB on a 1 GB hub, at the shed threshold); one per chatter is ~700k
  (~19 GB, not a tuning problem).
- [Cache-aware PoP skipping](/quest/m2/pop-skipping/README.md) densifies the
  relay mesh, roughly tripling average degree on the topology it was designed
  against, and its warm advertisement adds a second, more specific route per
  carried broadcast. Both multiply whatever a route costs, which is now a table
  entry rather than an object graph.

### Expected result

The published table compared today, waiter slots, and standby routes at ~29 KB,
~19 KB, and ~9 KB per cluster-wide broadcast on a degree-5, 1 GB relay. Standby
routes has since landed by another route, and the baseline it was measured
against no longer exists, so the remaining claim is only that waiter slots
removes 840 B from every kio channel. Rebuild the harness and restate the table
before quoting a shed threshold.

### Reproducing the measurements

Two throwaway `moq-net` examples drive an origin and read `/proc/self/statm`
alongside a counting global allocator, with `N` paths, `ROUTES` sources per
path (distinct second hops so they splice onto one front), and `PEERS` announce
cursors. Neither is committed: they need `#[doc(hidden)]` size probes on private
types, and the numbers above are what they were written to produce. Rebuild them
from this quest rather than expecting them in the tree.

## Quests

- [Waiter slots](/quest/m2/relay-memory/waiters.md) - kio channels stop paying
  840 B of empty inline waker slots: the largest single lever in the questline
- [Routes per broadcast gauge](/quest/m2/relay-memory/route-gauge.md) - expose
  how many routes a relay holds for a path

## Related

- [Group charge](/quest/m0/group-charge.md) - the group cache charges what a
  cached group really costs; it undercounts chat-shaped traffic 4x today
