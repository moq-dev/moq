# [M] Local origin

## Goal

A localhost worker (a Voice agent, a recorder) connects to the relay's
internal listener and gets a moq session whose origin holds only the
broadcasts this relay ingested from its own customer sessions, never those
learned from cluster peers. Workers stop electing on hop chains ("the first
internal hop is me"), which [Cluster routing](/quest/m1/cluster-routing.md)
removes inside a cluster.

## Plan

- The relay already knows each route's arriving session and its tier: cluster
  peers attach through `origin.peer()` (`rs/moq-relay/src/cluster.rs`). Build
  an `origin::Consumer` that admits only routes from customer-tier sessions,
  without copying the table.
- The internal listener (`rs/moq-relay/src/internal.rs`) is plain HTTP
  today (`/metrics`, `/health`, `/nodes`, `/sessions`). Serve a moq session on
  it over the WebSocket transport the relay already accepts. It is
  unauthenticated, so serve it only when the internal listener binds a
  loopback address. The listener may also bind a private-overlay address,
  which would expose customer media to the overlay; config load fails if the
  local origin is enabled there.
- The session is read-only. Open question: Voice publishes responses. Decide
  whether this session may publish into the relay origin, and under which
  grant, before it accepts a publish.
- Document it in `doc/bin/relay/`.

Tests: a two-relay cluster where each relay's local session lists only its own
publishers, a publisher moving relays moves between the two views, and a
peer-learned route never appears.

## Related

- [Voice on the local origin](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/voice-local-origin.md) - moq.pro's Voice and recorder switch to this
