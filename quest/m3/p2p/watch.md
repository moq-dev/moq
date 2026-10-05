# [S] Watch opts in

## Goal

One attribute turns P2P on in the demo. A watcher tab keeps subscribing
through the shared origin, its subscriptions migrate to a direct peer that is
the broadcast's origin or a routing node, and they return to the relay when
the peer goes away, with no visible interruption.

## Plan

`hang-watch` and `hang-publish` gain a `p2p` attribute that constructs
`Peers` on the shared connection's origin with the demo's ICE servers and a
`max` from the page; `demo/web` exposes the toggle. The route pick is the
origin's, under the rule from [Direct peers win](/quest/m3/p2p/cost-scopes.md):
a direct link to the origin or a routing node wins, and losing it falls back
to the relay.

Two topologies and the demo shows both: a publishing tab serving watcher tabs
directly, and watcher tabs pulling from a `moq-cli --p2p` hop.

## Required

- [Signaling and policy](/quest/m3/p2p/signal.md)
- [moq-cli joins](/quest/m3/p2p/cli.md) - the native hop the second topology shows
- [Direct peers win](/quest/m3/p2p/cost-scopes.md)
