# [S] Watch opts in

## Goal

One attribute turns P2P on in the demo. A watcher tab keeps subscribing
through the shared origin, its subscriptions migrate to a peer when the
origin ranks that route cheaper, and they return to the relay when the peer
goes away, with no visible interruption.

## Plan

`hang-watch` and `hang-publish` gain a `p2p` attribute that constructs
`Peers` on the shared connection's origin with the demo's ICE servers and a
`max` from the page; `demo/web` exposes the toggle. The route pick is the
origin's, from [cost ranking](/quest/m2/route-cost.md) under the rule from
[cost across scopes](/quest/m2/p2p/cost-scopes.md): a peer already carrying
the broadcast wins, and its retraction falls back to the relay.

Three topologies and the demo shows all of them: a publishing tab serving
watcher tabs directly, watcher tabs pulling from a `moq-cli --p2p` hop, and a
watcher tab re-serving to another watcher through
[transit](/quest/m2/p2p/transit.md).

## Required

- [Signaling and policy](/quest/m2/p2p/signal.md)
- [moq-cli joins](/quest/m2/p2p/cli.md) - the native hop the second topology shows
- [Transit in the JS origin](/quest/m2/p2p/transit.md)
- [Route cost in the JS origin](/quest/m2/route-cost.md)
- [Cost across scopes](/quest/m2/p2p/cost-scopes.md)
