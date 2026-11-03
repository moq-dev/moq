# [S] Watch opts in

## Goal

One attribute turns P2P on in the demo. A watcher tab keeps subscribing
through the shared origin, its subscriptions migrate to a direct peer that is
the broadcast's origin or a routing node, and they return to the relay when
the peer goes away, with no visible interruption.

## Plan

Publish `@moq/p2p` with its first application consumer in this quest. It is
currently private and has no release script; the `test/p2p` harness uses its
workspace source. Before its first workflow publish, a maintainer claims
`@moq/p2p` on npm and JSR with a placeholder and attaches the `moq-dev/moq`
trusted publisher, as [Claim new npm names](/quest/m2/release/npm-first-publish.md)
does for the cut's packages.

`moq-watch` and `moq-publish` gain a `p2p` attribute that constructs
`Peers` on the shared connection's origin and its relay auth handle (see
[Signaling and policy](/quest/m2/p2p/signal.md)), with the demo's ICE servers
and a `max` from the page; `demo/web` exposes the toggle and an explicit application
peer-selection policy. Enabling discovery alone must not dial every peer. The route pick is the
origin's, under the rule from [Peer route policy](/quest/m2/p2p/cost-scopes.md):
application priority then total cost selects among eligible connections.
The CDN session remains, but unused CDN payload demand is released. Same-epoch
handover resumes; epochless source changes restart. A direct link need not win.

Two topologies and the demo shows both: a publishing tab serving watcher tabs
directly, and watcher tabs pulling from a `moq-cli --p2p` hop.

## Required

- [Signaling and policy](/quest/m2/p2p/signal.md)
- [moq-cli joins](/quest/m2/p2p/cli.md) - the native hop the second topology shows
- [Peer route policy](/quest/m2/p2p/cost-scopes.md)
