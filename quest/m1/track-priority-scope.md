# [L] Scope track priority

## Goal

Priority orders streams only within one scheduling domain: the streams a single
party owns both ends of, the first mile from publisher to ingest relay and the
last mile from edge relay to viewer. On a shared relay-to-relay session the
domain is the broadcast: when the relay enables it, broadcasts share the link
fairly and priority orders streams only inside each broadcast, so one
customer's viewers asking for 255 never starve another customer's broadcast.

## Plan

Where the model stands today, all in `rs/moq-net`:

- One `PriorityQueue` per moq-lite session (`lite/publisher.rs`), a strict
  global sort over every in-flight group on the connection: a subscription at
  priority 200 pre-empts one at 100 indefinitely, and only the top 255 groups
  get a distinct rank.
- `Priority` already ranks a group by its track priority, then its subscription
  id, then newest group first within that subscription (`lite/priority.rs`). JS
  packs track priority and the group's position within its own subscription
  into one send order (`js/net/src/lite/priority.ts`), so two tracks at equal
  priority interleave rather than one draining first.
- The draft fixes group order within a track: newest first, with no wire field
  to invert it (`drafts/draft-lcurley-moq-lite.md`, Prioritization).
- A relay forwards the max of its downstream subscriber priorities upstream
  (`model/subscription.rs`, `lite/subscriber.rs`), never the publisher's track
  priority, so one viewer asking for 255 raises that track above every other
  tenant's on the cluster link (`rs/moq-relay/src/cluster.rs`: one
  bidirectional session per peer pair carries every broadcast). The draft
  already says the upstream leg SHOULD use the publisher priority.

Decided 2026-09-19: the domain on a relay-to-relay session is the broadcast,
keyed by its path, and it is a relay setting (`cluster.fair = true`), off by
default so a single-tenant deployment keeps strict priority across the link.
A tenant-keyed domain (one bucket per customer, however many broadcasts they
run) needs the tenant identity from the auth line first and is a later
refinement of the same tier, not a different design. Opening more
subscriptions or broadcasts must never buy more bandwidth than the bucket
allows.

Direction to settle in the draft first, then the code:

- On the last mile the viewer owns the whole session, so its audio and video
  subscriptions share one domain and the wire is unchanged. On a relay-to-relay
  session the relay knows every subscription's broadcast, so no carrier is
  needed on the wire; the draft only has to say that a relay MAY scope
  priority to the broadcast.
- On a relay-to-relay session with fairness on, ordering across broadcasts is
  byte-fair (the scheduler's fair tier, one send group per broadcast), the
  publisher's track priority orders streams within a broadcast, and
  downstream subscriber priorities are not forwarded. Attribute FETCH streams
  to a domain the same way: cache-miss and history fetches carry Subscriber
  Priority outside any subscription, so leaving them outside every bucket
  bypasses tenant isolation; settle their carrier beside the subscription one,
  or narrow the goal to subscription delivery.
- Keep the current ranking: track priority, then subscription, then newest
  group; do not reintroduce a group-order direction knob.
- A per-session cap on distinct ranks is a scheduling detail; whatever replaces
  the 255-entry sort must stay O(log n) per group under chat-shaped churn.

Prove with the existing lite publisher tests extended to two broadcasts on
one session with fairness on: neither starves, a downstream 255 does not
change the upstream order, and with fairness off the strict order is
unchanged. The io_uring workers apply the same setting. Update `doc/concept/moq-lite.md`, whose priority table still
describes a direction knob the wire does not carry. Public API impact: the
meaning of `Subscribe.priority` on a cluster session; report it with the draft
change.

## Required

- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - the fair
  tier the per-broadcast send groups ride on

## Related

- [Starvation](/quest/m1/qos/starvation.md) - the relay-side signal that
  shows a starved subscription
