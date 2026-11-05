# [L] Fair share across broadcasts on cluster links

## Goal

On a shared relay-to-relay session, broadcasts share the link fairly and
priority orders streams only inside each broadcast, so one tenant's viewers
asking for the highest priority never starve another tenant's broadcast.
Opening more subscriptions must never buy a broadcast more than its bucket
allows. Until tenant keying lands, a peer that opens more broadcasts does get
more shares, on send and receive alike; tenant keying is what closes that.
Single-tenant deployments and the first and last mile keep
strict priority across the session, as today.

## Plan

Needs a fresh `/quest-plan` before starting: there is no settled design.
Every transport already shares moq-quic's scheduler since qmux moved onto it
([moq-dev/moq#6231](https://github.com/moq-dev/moq/pull/6231)); revisit once the
auth line carries a tenant identity.

Today every session schedules strictly: one `PriorityQueue` or one moq-quic
send group per subscription, ranked by the subscriber's priority, then the
publisher's track priority, then subscription id
(`rs/moq-net/src/priority.rs`). A relay forwards the max of its downstream
subscriber priorities upstream (`model/subscription.rs`, `lite/subscriber.rs`,
and on moq-transport upstreams `ietf/subscriber.rs`), and
`rs/moq-relay/src/cluster.rs` runs one bidirectional session per peer pair
carrying every broadcast, so a viewer asking for 127 raises its track above
every other tenant's on the cluster link.

Direction carried over from 2026-09-19, not re-confirmed:

- The domain on a relay-to-relay session is the broadcast, keyed by its path,
  behind a relay setting (`cluster.fair`), off by default.
- A tenant-keyed domain (one bucket per customer, however many broadcasts they
  run) needs the tenant identity from the auth line and refines the same tier.
- moq-quic had the fair tier: send domains sharing bytes by deficit round
  robin, with groups inside them. [the API trim](https://github.com/moq-dev/moq/pull/6809)
  deletes it while nothing uses it (maintainer, 2026-10-23); restore it from
  that PR's history with this quest's consumer.
  Exposing them gives `transport::poll::Session::send_group` a domain, a
  trait break for every implementer.
- The draft would say a relay MAY scope priority to the broadcast on a
  relay-to-relay connection; no carrier is needed on the wire.
- Decided 2026-10-20 (maintainer): the `transport::poll` trait breaks land in
  order, receive leases ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)), then
  [native request admission](https://github.com/moq-dev/moq/pull/6409), then the send domain.
  Both earlier breaks have landed, so the send domain waits on nothing in that order.

Receive ranks, folded in from
[moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)'s follow-ups by
the maintainer on 2026-10-20. When the receive window is stuck, #5878 evicts
the lowest subscription priority first across every broadcast on the session,
then the group its subscription would deliver last, and sets a stream's rank
once, when its header decodes:

- With fairness on, rank received groups within their broadcast's domain
  first, as sends are, so one broadcast's priorities never evict another's
  partial groups. Received FETCH streams map to a domain through the same
  carrier the send side settles.
- Ranking inside domains still leaves the session-wide window to share, so a
  broadcast that fills it would block every other one. Add a fair domain tier
  ahead of the ranks, mirroring the byte-fair send tier: a stuck window evicts
  from the domain holding the most beyond an even share of the window across
  the domains with partial groups, then by rank within that domain. A
  broadcast that fills the window alone may keep it until another arrives. A
  full window admits no newcomer's bytes, so settle what enters a broadcast
  into the fair set first, and test that it gets in.
- Re-rank a receive stream when its subscription's priority or group order
  changes, instead of keeping the rank from its header. This one does not
  need fairness; split it out if it should land first.

Dropped 2026-10-20 from
[moq-dev/moq#6142](https://github.com/moq-dev/moq/pull/6142), which landed
only signed priorities and the publisher-priority tiebreak. Its first
revision prototyped a send domain per broadcast in the lite publisher
(`session::Schedule { Strict, Broadcast }`, `poll::Session::send_domain`,
public moq-quic domains, `cluster.fair`), falling back to strict on a
transport without domains. The maintainer: "IDK I feel like we don't have a
good plan here". Open questions it left:

- The IETF publisher schedules strictly; it needs the same domains.
- Tenant-keyed domains instead of, or above, broadcast domains.
- FETCH placement: cache-miss and history fetches carry Subscriber Priority
  outside any subscription, so leaving them outside every bucket bypasses
  isolation.
- Subscriber aggregate forwarding: under a fair schedule the downstream max
  should stop being forwarded upstream at its source, not just be ignored by
  the next fair hop.
- Transports without send domains (the browser backend) and
  moq-transport sessions: refuse, or degrade to strict.
- A cheaper capability probe than opening and dropping a domain.

Prove it with two broadcasts on one cluster session over a bufferbloated
link: neither starves, a downstream 127 does not change the upstream order,
and with fairness off the strict order is unchanged. On the receive side, a
high priority in one broadcast never makes a stuck window evict another
broadcast's partial groups first, a broadcast that filled the window before
another began receiving yields down to its even share, and a priority or order
update moves an already-ranked stream.

## Related

- [One parked group per subscription](https://github.com/moq-dev/moq/pull/6140) - the parked group each subscription keeps must still yield the next stream by broadcast
