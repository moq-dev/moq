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

- Recommended: link the two relays for the overlap. The handoff connection
  between them already exists; the new relay runs a moq-lite session to the
  old one over a socket it carries, and each treats the other as a cluster
  peer until the old one exits, so routes cross between generations as they
  cross between nodes. The two share only the node's stats name
  (`rs/moq-relay/src/handoff.rs`): each process routes under its own random
  Relay ID (`origin::Config::default`, which `Cluster::new` uses), so
  lite-07's rule that a node never uses a route whose Path names its own
  relay treats them as two routing nodes, and neither refuses the other's
  routes. A test pins that, so a later change that shares the Relay ID
  across a handoff fails it.
- Rejected: keep a peer's old session past idle until the old relay closes it.
  That holds the old relay to its drain deadline again, which is what handoff
  removes.

Exercise it with a publisher that ignores GOAWAY on the old relay and a
subscriber on another node, past `goaway.handover`.

Picked by the maintainer on 2026-11-15: ✅ keep this quest in m4 (rejected:
drop it), and ✅ the relay-restart PR landed without it (rejected: hold that PR
for this link).
