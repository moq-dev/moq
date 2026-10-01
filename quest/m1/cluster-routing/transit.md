# [M] Non-transit relays

## Goal

A relay can be configured never to carry routes between its cluster links,
which is what lets an operator build edge and core tiers out of one relay
with no roles. A relay with `cluster.transit` off advertises only what
entered locally to its cluster links and never re-advertises a route learned
from one link to another. It still serves its own clients every route it
knows. Edge, core, and one-node region are an operator's names for
configurations, not relay modes.

## Plan

Decided 2026-10-01, replacing the edge and core roles the 2026-09-30
wildcard audit (cache tiers) planned:

- `cluster.transit` is the node's own setting, on by default, from config or
  alongside the peer list the connect API returns. Off, every cluster-peer
  session publishes only `origin::Consumer::local()`, accepted as well as
  dialed (today they take different paths in `cluster.rs`), so no cluster
  link routes through it whichever side dialed. The grant's peer
  classification tells a peer from a client, and clients still see
  everything it learned from its links.
  A typical edge runs with it off and dials its region's cores; a core or a
  one-node region runs with it on.
- No roles. Who dials whom is the peer list, and whatever generates the list
  applies the operator's rules (an edge dials its region's cores, cores dial
  only lower-named cores of linked regions). The relay does not check its
  links against a topology; the generator is where that is tested.
- Refusing clients on a hidden node is admission, not routing: the auth
  server or embedder already decides every session and sees its verified
  client certificate, so it refuses a session that is not a cluster peer.
  The relay adds no client switch.
- Relays from before this setting are transit, so a cluster migrates one
  region at a time with no legacy mode. A permanently mixed-version cluster
  stays a non-goal.
- Spreading paths over equally ranked links by the path-keyed rendezvous
  hash is [Selection](/quest/m1/cluster-routing/selection.md)'s, for every
  relay. With transit off it is what makes every edge of a region send a
  given path to the same core.
- Edge links run qmux over TLS on TCP, not WebSocket and not QUIC:
  intra-region links are not congested. qmux over TCP exists today only in
  plaintext (`tcp://`, moq-tokio's `tcp.rs`); add `tls://` beside it on
  moq-tokio's `tls` helpers.
  (Scheme name decided 2026-09-30.)

Test: a two-region cluster in `rs/moq-relay/tests`. One region has two
non-transit relays dialing two transit relays; the other is one transit relay
that also serves clients. Subscribers on both non-transit relays cause one
cross-region subscription per broadcast, a route from one transit relay never
reaches the other through a non-transit relay, a client on the far relay
gets a broadcast published on a non-transit relay, a transit relay that
dials a non-transit relay still gets no routes through it, and losing a
transit relay moves only its paths.

Public API: `cluster.transit` (default on) and a TLS qmux URL scheme.
`cluster.tier` is unrelated: it stays the billing and stats label.
Wire: none expected.

## Related

- [moq-transport peers are plain clients](/quest/m1/ietf-cluster-off.md) - cluster links are lite-only
