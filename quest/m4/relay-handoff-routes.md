# [L] Keep cluster routes for sessions that stay on a draining relay

## Goal

During a [handoff](/doc/bin/relay/config.md), a session that stays on the old
relay through its drain, such as a third-party client that ignores GOAWAY,
keeps reaching the cluster, and the cluster keeps reaching it. Today the
cluster peers that dialed the old relay redial the new one at once and retire
their old session once it carries no subscription, or after `goaway.handover`
(10 s by default). From then on a broadcast published only on the old relay is
unreachable from the rest of the cluster, and a subscriber left on the old
relay loses every broadcast it reached through those peers, until the old
relay's drain ends.

## Plan

Picked by the maintainer on 2026-11-15: ✅ keep this quest in m4 (rejected:
drop it), and ✅ the relay-restart PR landed without it (rejected: hold that PR
for this link). Rejected the same day: keep a peer's old session past idle
until the old relay closes it, which holds the old relay to its drain
deadline again, the thing handoff removes.

The recommended design, linking the two relays as cluster peers over the
handoff socket for the overlap, does not survive lite-07's route rule as
shipped: the two generations share the relay's identity at different
Starts, and a node never uses a route whose Path names its own relay at any
Start (the draft's Routing), so each would refuse the other's routes. What
replaces it is the maintainer's call (Waiting).

Exercise it with a publisher that ignores GOAWAY on the old relay and a
subscriber on another node, past `goaway.handover`.

## Waiting

- Maintainer: how the two generations link, now that a routing link between
  them is refused. Options: (a) recommended, the old relay dials the new one
  as a plain endpoint over the handoff socket, declaring no Node ID, so the
  new relay announces the old one's local claims under its own Node ID and
  serves the old one's subscribers from the cluster, with no wire change;
  (b) narrow the rule to refuse only a path naming the node's own Start,
  a wire change that also lets a restarted relay take routes through its
  dead predecessor until the session floor refuses them; (c) drop the quest,
  accepting the gap for clients that ignore GOAWAY. Check: this item is
  replaced by the pick in the Plan.
