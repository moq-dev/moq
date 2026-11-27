# QUIC multipath

## Goal

One moq-quic connection runs over several network paths at once, as
[RFC 10069](https://www.rfc-editor.org/rfc/rfc10069) specifies, so a
multi-homed publisher (Wi-Fi plus cellular, or several modems) bonds its
links: the session survives any one link dying with no redial, and its rate
can exceed one link's. Two moq-quic endpoints negotiate it in the handshake
when both opted in, and a connection that did not negotiate it is unchanged
on the wire. Linux opens extra paths first; every platform accepts them.

Non-goals: browsers and qmux sessions, which have no QUIC paths to add;
relay-to-relay links; any moq-lite or moq-transport wire change; forward
error correction; and choosing which interfaces to use, which is the
application's.

## Plan

Decided 2026-11-26 (`/quest-plan` via kixel-ai/kixel.ai). This reverses
"Multipath is deleted" from the fork plan of 2026-09-30
([moq-dev/moq#4617](https://github.com/moq-dev/moq/pull/4617)), which dropped
it because no MoQ connection negotiated it. The base stays quinn; only this
extension comes back, now with a consumer.

- ✅ Bonding: several paths carry traffic at once (rejected: failover only,
  one active path with validated standbys; dropping the work).
- ✅ Striping first: each packet goes once, on the lowest-RTT path with
  window room, and data lost on one path is retransmitted on another. The
  scheduler is designed as configurable, and
  [redundancy](/quest/m4/quic-multipath-redundancy.md) follows (rejected:
  striping with an opt-in duplicate mode now; duplicating everything, which
  multiplies bytes and adds no capacity).
- ✅ Redundancy is one later quest: a per-stream property with a connection
  default, which covers duplicating everything and mixing both (rejected: a
  connection-wide switch first and per-stream later, whose switch the second
  quest rebuilds; connection-wide only).
- ✅ Opt-in on both ends, off by default: each end sets a maximum path count
  under `[quic]` (rejected: servers accept by default while clients opt in;
  on everywhere). It changes packet numbering and acknowledgments on every
  connection that negotiates it, and a server holds connection IDs per
  allowed path for each client.
- ✅ Linux opens extra paths first. Android and iOS follow at medium
  priority, wanted more for failover than for duplication, then macOS and
  Windows at lower priority, each its own quest (rejected: Linux and macOS
  now; every platform now).
- ✅ Mechanism here, policy in the application: moq-tokio opens, closes, and
  re-roles a path on a named interface and reports each path's state and
  stats, and its client config takes a static list. Discovering interfaces,
  selection rules, and following link changes are the application's.
- ✅ m3: kixel.ai's
  [bonded uplink](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m1/bond/uplink.md)
  waits on it.

Recommended, open to review:

- Port noq-proto's multipath by hand (moq-dev/noq, the same quinn lineage
  and license) rather than write it fresh, crediting it in
  `rs/moq-quic/README.md`. Implement RFC 10069's final codepoints only, no
  draft versions.
- The fork's quinn syncs get harder wherever multipath reshapes packet
  spaces and loss recovery. Record that under "Carried changes", so a sync
  knows which upstream fixes need adapting by hand.
- Each path keeps its own congestion controller, as `PathData` already
  does, uncoupled. Bonded links rarely share a bottleneck; revisit only if a
  shared-bottleneck measurement shows the connection taking more than its
  share.
- `Path` in moq-net is a broadcast's name. Inside moq-quic, `path` keeps
  QUIC's meaning; moq-tokio's public API namespaces the network path so the
  two cannot be confused.
- With [keep-alive](/quest/m1/keep-alive.md), liveness is per path: a path
  with no reply inside `soft` stops carrying traffic, and the session turns
  Stale only when every path has.
- Every path must reach the relay, and the worker, that holds the
  connection. Asked 2026-11-26, since few providers' balancers route by
  QUIC-LB:
  - A relay on its own unicast address, chosen by DNS at connect, needs
    nothing, provided the client reuses the address the handshake reached
    for every path and never resolves again per interface. A geo DNS answer
    over one network can name a different relay than over another.
  - Workers on one host are already steered by connection ID
    (`SO_REUSEPORT`), as migration needs.
  - Relays behind one anycast address or VIP forward a packet to its owner
    by the server ID in its connection ID (`[listen.lb.forward]`), so a
    plain 5-tuple balancer works and a QUIC-LB-aware one only saves the hop.
    The io_uring runtime refuses `lb.id` today.
  - So the connection IDs issued for every path carry the same steering
    byte and server ID as the first path's.
  - A path of the other address family goes only to the relay's
    `preferred_address` of that family, the one other address known to be
    the same relay.

The children run in order: [the protocol](/quest/m3/quic-multipath/core.md),
[paths on Linux](/quest/m3/quic-multipath/paths-linux.md), then
[striping](/quest/m3/quic-multipath/stripe.md).

The epic's own work, once they land: a `doc/` guide to bonded contribution
that says what it needs on both ends, when a backup path carries traffic,
and how to read per-path stats.

## Related

- [QUIC address discovery](/quest/m3/quic-address-discovery.md) - reports an observed address per path
- [A relay dials from its listening socket](/quest/m3/relay-one-socket.md) - extra paths leave from the one socket too
- [A path that's back is used within an RTT](/quest/m1/quic-path-return.md) - the single-path half of surviving an outage
- [moq-quic's client migrates to a server's preferred address](/quest/m4/quic-preferred-address.md) - where extra paths go when a relay advertises one
