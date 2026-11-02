# Mesh libraries

## Goal

A Rust application or a relay node enrolls into a mesh served by a relay
(self-hosted with `moq auth`, or a hosted coordinator implementing the same
contract) and reaches any other node by its Ed25519 endpoint ID, through
relays or directly. Nodes that can dial each other (one LAN, a shared VPC or
Docker network, a public address) talk directly over a sparse, deterministic
overlay that forwards for the rest; everything else, and any partition of
the overlay, goes through relays. Content is signed by default, and each node
publishes a `.status` track describing its links.

This repository ships the libraries. The mesh binary and its UI live outside
it: [kixelated](https://github.com/kixelated) builds a `kixel` daemon on these
libraries, and any other binary can do the same.

Non-goals: a mesh daemon or CLI here (`moq mesh` is not added to moq-cli),
MagicDNS, agent protocols, a graph UI, internet P2P or NAT traversal in m2
(remote traffic always crosses a relay; [mesh over ICE](/quest/m3/mesh-ice.md)
is parked in m3), TUN devices or IP routing, encryption (parked in
[mesh keys](/quest/m3/mesh-keys.md)), a member list that a compromised
coordinator cannot extend, and Windows.

## Plan

Decided 2026-11-02 with the maintainer (Kixel's planning interview, which
moved moq.pro's hosted mesh and T3 Code lines to Kixel): **moq for
libraries, Kixel for the binary and UI.** This epic keeps the `Endpoint` API,
address hints, the sparse overlay, and the `.status` track, beside the byte
streams, sockets, federation, and `moq_auth::mesh` contract it builds on. The
daemon, MagicDNS, agents and their MCP server, the graph component, robot
builds and tests, and the user guide moved to Kixel. Why: Kixel is the brand
for non-media work on MoQ, and a mesh node is a moq-relay instance, so any
moq library, media included, can point at one.

Decided 2026-10-25 with the maintainer (planning interview), still in force:

- Traffic is app-level [byte streams](/quest/m2/stream/README.md) and named
  tracks, never IP packets.
- The coordinator contract is generic and lives here: enrollment issues a
  relay URL and a token, and self-hosters run it with `moq auth`. A hosted
  service implements the same contract with its own accounts and billing.
- Direct paths only to addresses a node can already dial: `[cluster.lan]`
  mDNS, plus [address hints](/quest/m2/mesh/addr.md) published on mesh
  tracks. No ICE, STUN, or hole punching; the [P2P](/quest/m2/p2p/README.md)
  line is separate.
- An endpoint ID is a persistent Ed25519 device key, iroh-style. It is not
  the [routing identity](/quest/m1/cluster-routing/routes.md), which each
  process declares fresh.
- Addressing is announcement: each node publishes under `mesh/<id>/...`, and
  reaching an ID is subscribing there. There is no separate directory.
- The token is the binding: enrollment proves the device key, then the
  coordinator issues a grant to publish `mesh/<id>/**` and subscribe
  `mesh/**`, plus publish on `mesh/*/*/.dial/<id>` so the node can dial
  another's [socket](/quest/m2/socket/README.md) under its own id (amended
  2026-11-01). Relays need no new check. Membership is the coordinator's
  token only; a Tailnet-Lock analog was declined.
- Protection is per broadcast, with levels that nest: identity (the token
  only), authentication (per-frame Ed25519 signatures, the default), and
  later encryption (signed plus moq-e2ee, waiting on
  [mesh keys](/quest/m3/mesh-keys.md)).
- Enrollment is automated, with no human in the loop (decided 2026-10-30): a
  device presents an enrollment key the owner created and then refreshes by
  proving its key. A hosted coordinator may put its own sign-in in front of
  minting that key.
- The API is iroh-shaped (not wire-compatible with iroh).
- It builds on [outbound federation](/quest/m1/cluster-routing/federation.md):
  a node's local relay dials its relays as `upstream` links and ranks LAN
  peers above them.

Decided 2026-10-31 with the maintainer (moq.pro's mesh planning interview):

- mDNS alone is not enough. It fails in Docker bridge and overlay networks,
  cloud VPCs, across subnets and VLANs, on isolated Wi-Fi, and for a macOS
  launchd agent denied Local Network access, so nodes publish
  [address hints](/quest/m2/mesh/addr.md).
- Direct links form a deterministic [sparse overlay](/quest/m2/mesh/overlay.md)
  computed locally from the member list, with an optional per-node degree
  cap. Every node keeps its relay uplink, which carries traffic across an
  overlay partition.
- `.status` carries each link's RTT, and an application embedding `Endpoint`
  publishes it too ([status track](/quest/m2/mesh/status.md)).

This epic owns the end-to-end test: three relay nodes with the mesh enabled,
two in network namespaces sharing a LAN segment and a third behind a relay
only, enrolled through `moq auth`; an `Endpoint` on each dials the others'
services, taking the LAN path between the first two and the relay path to
the third, and survives a relay restart.

## Required

- [Outbound federation](/quest/m1/cluster-routing/federation.md) - a local relay dials its relays and prefers LAN peers
- [Byte streams](/quest/m2/stream/README.md) - lossless TCP-level streams over MoQ
- [Endpoint API](/quest/m2/mesh/endpoint.md) - an iroh-shaped `Endpoint` that connects to an endpoint ID and service and accepts byte streams
- [Address hints](/quest/m2/mesh/addr.md) - nodes publish their addresses on mesh tracks and dial each other directly where mDNS cannot find them
- [Sparse overlay](/quest/m2/mesh/overlay.md) - each node keeps about log N deterministic direct links, forwards for the rest, and falls back to relays across a partition
- [Status track](/quest/m2/mesh/status.md) - a signed `.status` track per node with its links and per-link RTTs
- [Mesh library guide](/quest/m2/mesh/guide.md) - the doc.moq.dev page for the libraries, the contract, and porting from iroh

## Related

- [P2P](/quest/m2/p2p/README.md) - internet peer sessions, which the m2 mesh does not use
- [Mesh over ICE](/quest/m3/mesh-ice.md) - deferred direct paths between mesh nodes on different networks
- [End-to-end encryption](/quest/m2/e2ee/README.md) - the AEAD the encryption level will reuse
- [Teleoperation](/quest/m3/teleop-robot.md) - robot track shapes that could ride the mesh
