# [L] Scope track priority

## Goal

Priority orders streams only within one scheduling domain: the streams a single
party owns both ends of, the first mile from publisher to ingest relay and the
last mile from edge relay to viewer. A tenant's priority never orders another
tenant's streams on a shared relay-to-relay session.

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

Direction to settle in the draft first, then the code:

- Name the scheduling domain the priority field orders. On the last mile the
  viewer owns the whole session, so its audio and video subscriptions share one
  domain. On a relay-to-relay session one bidirectional connection carries
  every tenant's subscriptions, and the peer connection's authorization is the
  cluster peer's, not the viewers', so the domain must arrive per subscription.
  Settle the carrier in the draft: a subscription-scoped property the origin
  relay fills from the viewer's grant, or a relay-local attribution table keyed
  by subscription. Opening more subscriptions must never buy more bandwidth, so
  there is no per-subscription fallback; the fairness bucket lives in the
  scheduler's domain tier.
- On a relay-to-relay session, ordering across domains is fair (round-robin or
  weighted by domain), publisher priority breaks ties within a broadcast, and
  downstream subscriber priorities are not forwarded. Attribute FETCH streams
  to a domain the same way: cache-miss and history fetches carry Subscriber
  Priority outside any subscription, so leaving them outside every bucket
  bypasses tenant isolation; settle their carrier beside the subscription one,
  or narrow the goal to subscription delivery.
- Keep the current ranking: track priority, then subscription, then newest
  group; do not reintroduce a group-order direction knob.
- A per-session cap on distinct ranks is a scheduling detail; whatever replaces
  the 255-entry sort must stay O(log n) per group under chat-shaped churn.

Prove with the existing lite publisher tests extended to two scheduling domains
on one session: neither starves, and a downstream 255 does not change the
upstream order. Update `doc/concept/moq-lite.md`, whose priority table still
describes a direction knob the wire does not carry. Public API impact: the
meaning of `Subscribe.priority` on a cluster session; report it with the draft
change.

## Required

- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - the domain
  tier this scoping's fairness buckets ride on

## Related

- [Starvation](/quest/m2/qos/starvation.md) - the relay-side signal that
  shows a starved subscription
