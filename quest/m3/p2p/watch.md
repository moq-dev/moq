# [S] Watch opts in

## Goal

One attribute turns web P2P on in the demo. A watcher tab keeps subscribing
through the shared origin and reaches each broadcast over the cheapest route
the origin knows, relay or LAN peer.

## Plan

`hang-watch` and `hang-publish` gain a `p2p` attribute that constructs
`Peers` on the shared connection's origin; `demo/web` exposes the toggle. The
route pick is the origin's, from
[cost ranking](/quest/m2/route-cost.md): a LAN peer announces at cost zero
with a shorter hop chain than the relay path, so it wins, and its retraction
falls back to the relay.

Two topologies work without JS transit and the demo shows both: a publishing
tab serving watcher tabs on the LAN directly, and watcher tabs pulling from a
`moq-cli --p2p` hop.

## Required

- [Signaling over the relay](/quest/m3/p2p/signal.md)
- [Route cost in the JS origin](/quest/m2/route-cost.md)
