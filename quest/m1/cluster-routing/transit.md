# [M] Upstream links

## Goal

A cluster link can be marked upstream, and a relay never re-advertises a
route learned on one upstream link to another upstream link. Everything else
still transits. That one rule builds moq.pro's edge tier (an edge marks its
region's cores upstream, so it is never transit between them) and a drone
uplink (a Starlink drone marks its CDN link upstream: the mesh reaches the
CDN and the CDN reaches the mesh, but the drone never carries CDN traffic
between CDN relays). Edge, core, and one-node region are an operator's names
for configurations, not relay modes. Clients still see every route a relay
knows.

## Plan

Decided 2026-10-01, replacing the per-node `cluster.transit` setting (#4683)
with a per-link attribute, because a drone must transit for its mesh but never
for its CDN uplink, which no per-node switch can express (BGP's
customer/provider export rule, RFC 9234, reduced to one bit):

- `upstream` is an attribute of a peer entry, from config or the connect
  API's peer list, off by default. It applies to the link whichever side
  dialed: an edge that is dialed by a core still treats that core as
  upstream, so accepted peer sessions need a way to be marked (the peer's
  grant classification or identity is the likely source; pick the simplest
  shape that covers the reverse-dial test).
- An upstream link is sent only routes that did not arrive on another
  upstream link. On today's path vector that is the routes that entered
  locally or over non-upstream links.
- The down-only mark that stops a two-uplink mesh from leaking CDN routes
  back into the CDN needs a wire bit, so it lands with
  [Routes and announces](/quest/m1/cluster-routing/routes.md), not here.
- No roles and no topology check. Who dials whom is the peer list, and
  whatever generates it applies the operator's rules (an edge dials its
  region's cores, cores dial only lower-named cores of linked regions); the
  generator is where that is tested.
- Refusing clients on a hidden core is admission, not routing: the auth
  server or embedder refuses a session that is not a cluster peer.
- Relays from before this attribute treat every link as transit, so a cluster
  migrates one region at a time with no legacy mode.
- Spreading paths over equally ranked upstream links by the path-keyed
  rendezvous hash is [Selection](/quest/m1/cluster-routing/selection.md)'s;
  it is what makes every edge of a region send a given path to the same core.
- Edge links run qmux over TLS on TCP, not WebSocket and not QUIC:
  intra-region links are not congested. qmux over TCP exists today only in
  plaintext (`tcp://`, moq-tokio's `tcp.rs`); add `tls://` beside it on
  moq-tokio's `tls` helpers. (Scheme name decided 2026-09-30.)

Test: a two-region cluster in `rs/moq-relay/tests`. One region has two edges
whose links to two cores are upstream; the other is one relay that also
serves clients. Subscribers on both edges cause one cross-region subscription
per broadcast, a route from one core never reaches the other through an edge,
a client on the far relay gets a broadcast published on an edge, a core that
dials an edge still gets no routes through it, and losing a core moves only
its paths. Add a three-node line (mesh node, uplink node, CDN relay) where the
mesh node's broadcast reaches the CDN and a CDN broadcast reaches the mesh
node.

Public API: an `upstream` peer attribute and a TLS qmux URL scheme.
`cluster.tier` is unrelated: it stays the billing and stats label.
Wire: none.

## Related

- [moq-transport peers are plain clients](/quest/m1/ietf-cluster-off.md) - cluster links are lite-only
