# [L] Standby routes

## Goal

A route a relay never selects costs a table entry rather than a full broadcast
object graph. Degree stops being a straight multiplier on announce memory, which
is what makes hub relays and the denser [PoP-skipping
graph](/quest/m2/pop-skipping/README.md) expensive.

## Plan

### What actually happens today

A relay of degree `d` hears about the same broadcast `d` times: once over the
shortest path and once forwarded by each other neighbor, because split horizon
only suppresses a chain that already passed through us. Every one of those
arrivals runs `Subscriber::start_announce` -> `origin::Producer::create_broadcast`,
which mints a `broadcast::Producer` (two kio state cells plus an `Alive`) and a
lifecycle task, and then `attach_source` files it as one more `FrontRoute` on the
single shared front for that path.

Only the best route by `route_order` is ever `active`. The other `d - 1` object
graphs exist to hold three facts: this peer also has it, at this cost, via this
hop chain. Measured, that is 4.3 KB each today and ~2.9 KB after [waiter
slots](/quest/m2/relay-memory/waiters.md).

### The change

Represent a non-selected route as a record: hop chain, cost, announce id, and a
handle to the session that would serve it. Roughly 250 B. Mint the
`broadcast::Producer` and attach it only when the route is promoted (failover,
or a cheaper route arriving) or first subscribed. Promotion stays local: the
session already exists, so no network round trip is added to failover.

### What has to keep working

- `route_order` ranks on announce flag, cost, hop length, the `fnv_key` hash,
  and attach recency. All of those are in the record, so ranking must not need
  the producer.
- Split horizon and `FrontState::excluded`: exposure is registered when a path
  is advertised to a peer, not only when they subscribe, and `taints_a_reader`
  decides on hop chains. Confirm a lazy standby still participates, because
  getting this wrong hands a peer back its own bytes.
- Lite-06 announce ids map to paths per stream (`announced_by_id`). A record
  keeps that mapping without the producer; `ANNOUNCE_RESTART` must still
  re-price a standby in place.
- `AnnouncedRoute` currently holds a `broadcast::SourceGuard` from
  `create_broadcast`, and dropping it is how a retraction detaches. The record
  needs the equivalent teardown with nothing attached.

### Boundaries

Regression test that a broadcast announced by `d` peers attaches one source and
`d - 1` records, and that killing the active peer promotes a record without a
gap. The failover path is the one that gets slower if this is done wrong, so
prove it rather than asserting it.

Add a routes-per-broadcast accessor in the same change: this code is already
open, and moq.pro's (downstream) announce gauge wants it, so it rides this PR
rather than becoming a change of its own.
