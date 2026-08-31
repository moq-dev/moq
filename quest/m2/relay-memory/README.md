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

The per-route half is separate: every received announcement runs the whole
`create_broadcast` path, minting its own `broadcast::Producer` and lifecycle
task, even for a standby route that is never selected. In a sparse mesh a relay
of degree `d` attaches `d` sources to one front, so degree is a straight
multiplier. [Standby routes](/quest/m2/relay-memory/standby-routes.md) is that half.

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

`dev` (`b39fb87`) does not help: it is 10-13% worse per broadcast (9.7 KB at
one route, 4.8 KB per extra route) because the structs grew, and it still
requests announcements at prefix `""`. It does move the per-broadcast
lifecycle off two tokio tasks onto one shared `Driver`, which helps churn but
not footprint.

### Why now rather than later

Nothing is near the ceiling today. Two committed directions move it:

- Chat-shaped traffic (a moq.pro downstream direction) names announcement
  churn as the control-plane risk. One broadcast per channel is ~10k
  (~270 MB on a 1 GB hub, at the shed threshold); one per chatter is ~700k
  (~19 GB, not a tuning problem).
- [Cache-aware PoP skipping](/quest/m2/pop-skipping/README.md) densifies the
  relay mesh, roughly tripling average degree on the topology it was designed
  against. That multiplies exactly the per-route cost [standby
  routes](/quest/m2/relay-memory/standby-routes.md) removes, so landing that
  quest first keeps PoP skipping from tripling announce memory as a side
  effect.

### Expected result

Per cluster-wide broadcast on a degree-5, 1 GB relay, and the point at which
it sheds:

| | Per broadcast | Broadcasts |
|---|---|---|
| today | ~29 KB | ~7,000 |
| after [waiter slots](/quest/m2/relay-memory/waiters.md) | ~19 KB | ~11,000 |
| after [standby routes](/quest/m2/relay-memory/standby-routes.md) | ~9 KB | ~23,000 |

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
- [Standby routes](/quest/m2/relay-memory/standby-routes.md) - a route a relay
  never selects costs a table entry instead of a full broadcast object graph

## Related

- [Group charge](/quest/m0/group-charge.md) - the group cache charges what a
  cached group really costs; it undercounts chat-shaped traffic 4x today
