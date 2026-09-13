# [L] Scope track priority

## Goal

Priority decides send order only where a single party owns both ends of the
session, the first mile from publisher to ingest relay and the last mile from
edge relay to viewer, and only among the streams of one subscription. A
tenant's priority never orders another tenant's streams on a shared
relay-to-relay session.

## Plan

Where the model stands today, all in `rs/moq-net`:

- One `PriorityQueue` per moq-lite session (`lite/publisher.rs`), a strict
  global sort over every in-flight group on the connection: a subscription at
  priority 200 pre-empts one at 100 indefinitely, and only the top 255 groups
  get a distinct rank.
- A relay forwards the max of its downstream subscriber priorities upstream
  (`model/subscription.rs`, `lite/subscriber.rs`), never the publisher's track
  priority, so one viewer asking for 255 raises that track above every other
  tenant's on the cluster link (`rs/moq-relay/src/cluster.rs`: one
  bidirectional session per peer pair carries every broadcast). The draft
  already says the upstream leg SHOULD use the publisher priority
  (`drafts/draft-lcurley-moq-lite.md`, Prioritization).
- Ties break on the absolute group sequence across subscriptions, so a
  longer-running or clock-numbered track starves a newer one at equal priority
  (`lite/priority.rs` TODO). JS already ranks by position within the
  subscription (`js/net/src/lite/priority.ts`) and honours `ordered`; Rust
  ignores `ordered` in ranking.
- Rust control and announce streams sit at the transport default, tied with
  overflow groups and below every ranked group; JS puts protocol streams above
  all group data.

Direction to settle in the draft first, then the code:

- Subscriber priority orders streams within one subscription; across
  subscriptions on a last-mile session the viewer still owns both ends, so a
  viewer-scoped order remains legitimate there. State which scope the wire
  field means.
- On a relay-to-relay session, ordering across subscriptions is per-tenant
  fair (round-robin or weighted by subscription), publisher priority breaks
  ties within a broadcast, and downstream subscriber priorities are not
  forwarded.
- Rank by position within the subscription, honour `ordered`, and keep
  protocol streams above group data, as JS does.
- A per-session cap on distinct ranks is a scheduling detail; whatever
  replaces the 255-entry sort must stay O(log n) per group under chat-shaped
  churn.

Prove with the existing lite publisher tests extended to two tenants on one
session: neither starves, and a downstream 255 does not change the upstream
order. Public API impact: the meaning of `Subscribe.priority` on a cluster
session; report it with the draft change.

## Related

- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - the
  noq-side hierarchy this scoping rides on
- [Starvation](/quest/m2/qos/starvation.md) - the relay-side signal that
  shows a starved subscription
