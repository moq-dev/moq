# [M] Draining relays don't pull their peers back

## Goal

When a clustered relay drains, its cluster peers fail over to other routes
instead of redialing it, and they reconnect once its replacement is up. Peer
redials back off with jitter and a cap of tens of seconds, so a mesh
recovering from an outage doesn't redial in lockstep and a recovered peer
is not left waiting minutes. Fixed on `main` and backported to `release`.

## Plan

Found in the 2026-10-22 clustering audit, by reading code rather than by
reproduction:

- Drain sends an empty-URI GOAWAY to every accepted session
  (`rs/moq-relay/src/shutdown.rs`), inbound cluster peers included. The
  client treats an empty URI as "reconnect here" and redials the same node
  (`rs/moq-tokio/src/connection.rs`). Each new session replays both full
  announce tables for the rest of the drain window. GOAWAY also re-prices
  every route learned from that peer. During a serial rolling restart this
  repeats for every relay.
- The cluster dial loop in `rs/moq-relay/src/cluster.rs` doubles from 1 s to
  300 s with no jitter.

Reproduce first: a relay integration test with three relays under
controlled time, one draining, that counts sessions opened to the draining
relay and announce messages replayed during the drain. If peers do not
redial, delete the first half of this quest and record why.

Fix, if reproduced: a cluster peer that receives GOAWAY without a URI does
not redial the same node until its session closes. It then follows the
normal dial loop, so a restarted relay is picked up. Non-cluster clients
keep today's behavior. Add full jitter to the peer redial backoff and lower its
cap.

Tests: the reproduction above asserts no session reopens to the draining
relay while it drains, and one reconnect after it restarts. Backoff tests
assert spread and cap under controlled time.

Public API: none expected; report any connect config change. Wire: none.
Backport onto `release` as a separate PR.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - GOAWAY as lowest preference in the route layer
