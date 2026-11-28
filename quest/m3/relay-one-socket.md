# [L] A relay dials from its listening socket

## Goal

A tokio-runtime moq-relay sends and receives every QUIC connection, accepted
and dialed, on its one listening UDP socket, so the address a peer observes
for any of its connections is the address other peers can reach it at.
moq-tokio can dial from a server's listening endpoint, and moq-relay's
cluster dials always do.

## Plan

Decided 2026-11-15 (maintainer, `/quest-plan` from kixel.ai's
[hole punching](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m2/punch.md),
[kixel-ai/kixel.ai#116](https://github.com/kixel-ai/kixel.ai/pull/116)):

- ✅ Always one socket on the tokio runtime, reverting
  [moq-dev/moq#7688](https://github.com/moq-dev/moq/pull/7688)'s fresh UDP
  port per dialed session (rejected: shared by default with a fresh port
  after a peer's dial budget fails; opt-in per dial; opt-in per relay). The
  maintainer: "just always share a single socket. Fuck the bom issue we need
  another solution if that happens again." #7688 fixed a provider path that
  dropped one flow (`tyo0:443 -> bom0:51705`) for about 27 hours while fresh
  source ports got through; a stuck flow now holds that link down until it
  clears or the relay restarts, and recovering from one needs a different
  design if it recurs.
- ✅ Why it matters downstream: most NATs filter inbound packets by where the
  inside socket last sent, so a node's punch dials and the connection that
  reports its observed address
  ([QUIC address discovery](/quest/m3/quic-address-discovery.md)) must leave
  from the socket peers dial.
- ✅ An io_uring relay keeps its tokio cluster dials on their own sockets
  (rejected: porting dials to the ring now, which would end the measurement
  gate on [cluster dials on the ring](/quest/m5/uring-cluster-dial.md)).
  Document that it does not share its socket.
- ✅ m3 (rejected: m4).

Recommended: moq-tokio's QUIC `Server` hands out a dialer on its own
endpoint, which `Client` takes in place of binding a socket, rather than a
second endpoint type. `[connect] bind` no longer applies to cluster dials on
the tokio runtime; refuse a configured bind there instead of ignoring it.
Delete `tests/integration/cluster_fresh_port.rs` and the fresh-port text in
`doc/bin/relay/cluster.md` and `doc/bin/cli.md`'s `--connect-bind` note.

Tests: a relay's dialed and accepted sessions share one local address; two
relays that dial each other at once each hold their links on their listener;
redials reuse the socket.

Public API: additive in moq-tokio. Behavior: cluster dials leave from the
listener. Wire: none.
