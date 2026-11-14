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
it: Kixel builds a `kixel` daemon on these libraries, and any other binary
can do the same.

Non-goals: a mesh daemon or CLI here (`moq mesh` is not added to moq-cli),
MagicDNS, agent protocols, a graph UI, internet P2P or NAT traversal in m3
(remote traffic always crosses a relay; [mesh over ICE](/quest/m5/mesh-ice.md)
is parked in m5), TUN devices or IP routing, encryption (parked in
[mesh keys](/quest/m5/mesh-keys.md)), a member list that a compromised
coordinator cannot extend, and Windows.

## Plan

Moved from m1 to m3 2026-11-13 (maintainer, /quest-plan): it waits on m2
work, and no quest requires one in a later milestone, since a blocker ranks
by its own milestone. ✅ m3 (rejected: m2, where the cut would wait on it;
pulling the m2 blocker into m1). It ranks right after m2, when it can start.

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

- Traffic is app-level [byte streams](/quest/m1/stream/README.md) and named
  tracks, never IP packets.
- The coordinator contract is generic and lives here: enrollment issues a
  relay URL and a token, and self-hosters run it with `moq auth`. A hosted
  service implements the same contract with its own accounts and billing.
- Direct paths only to addresses a node can already dial: `[cluster.lan]`
  mDNS, plus [address hints](/quest/m3/mesh/addr.md) published on mesh
  tracks. No ICE, STUN, or hole punching; the [P2P](/quest/m4/p2p/README.md)
  line is separate.
- An endpoint ID is a persistent Ed25519 device key, iroh-style. It is not
  the [routing identity](/quest/m2/cluster-routes/README.md), which each
  process declares fresh.
- Addressing is announcement: each node publishes its address and status
  tracks under `mesh/node/<id>/...`, and reaching an ID is subscribing
  there. There is no separate directory. Services are not under a member
  subtree: each is one socket endpoint at `mesh/service/<service>` (decided
  2026-11-07, see [Endpoint API](/quest/m3/mesh/endpoint.md)).
- Certificates can stand in for the token (decided 2026-11-07, maintainer,
  Kixel's planning): a downstream mesh that must admit and evict devices
  with no internet issues long-lived certificates from its own signers, and
  [certificate credentials](/quest/m3/mesh/cert-credentials.md) checks them
  against a trust store it supplies. The coordinator's token stays the
  default for `moq auth`.
- The token is the binding: enrollment proves the device key, then the
  coordinator issues a grant to publish `mesh/node/<id>/**` and subscribe
  `mesh/**`, plus publish on its own [socket](/doc/concept/socket.md)
  endpoints, `mesh/service/*/.server/<id>` and `mesh/service/*/.client/<id>`,
  so it can accept and dial under its own id (amended 2026-11-01,
  2026-11-03 when sockets became endpoints, 2026-11-06 when endpoints split
  into server and client roles, and 2026-11-07 when member and service
  paths were scoped apart; the [Endpoint API](/quest/m3/mesh/endpoint.md)
  builds on the `mesh/node/<id>/**` and `mesh/service/*` grant). Relays need no new check. Membership is the coordinator's
  token only; a Tailnet-Lock analog was declined.
- Protection is per broadcast, with levels that nest: identity (the token
  only), authentication (per-frame Ed25519 signatures, the default), and
  later encryption (signed plus moq-crypto's e2ee, waiting on
  [mesh keys](/quest/m5/mesh-keys.md)).
- Enrollment is automated, with no human in the loop (decided 2026-10-30): a
  device presents an enrollment key the owner created and then refreshes by
  proving its key. A hosted coordinator may put its own sign-in in front of
  minting that key.
- The API is iroh-shaped (not wire-compatible with iroh).
- It builds on [outbound federation](/quest/m3/cluster-routing/federation.md):
  a node's local relay dials its relays as `upstream` links and ranks LAN
  peers above them.

Decided 2026-10-31 with the maintainer (moq.pro's mesh planning interview):

- mDNS alone is not enough. It fails in Docker bridge and overlay networks,
  cloud VPCs, across subnets and VLANs, on isolated Wi-Fi, and for a macOS
  launchd agent denied Local Network access, so nodes publish
  [address hints](/quest/m3/mesh/addr.md).
- Direct links form a deterministic [sparse overlay](/quest/m3/mesh/overlay.md)
  computed locally from the member list, with an optional per-node degree
  cap. Every node keeps its relay uplink, which carries traffic across an
  overlay partition.
- `.status` carries each link's RTT, and an application embedding `Endpoint`
  publishes it too ([status track](/quest/m3/mesh/status.md)).

This epic owns the end-to-end test: three [mesh relay nodes](/quest/m3/mesh/node.md),
two in network namespaces sharing a LAN segment and a third behind a relay
only, enrolled through `moq auth`; an `Endpoint` on each dials the others'
services, taking the LAN path between the first two and the relay path to
the third, and survives a relay restart.

Decided 2026-11-12 (maintainer, /quest-audit before the cut):

- ✅ (Superseded 2026-11-13: this epic is back in m3, above.) This epic and [byte streams](/quest/m1/stream/README.md) moved to m1
  from the head of m3 (rejected: staying in m3). The cut no longer waits on
  m1, and a downstream application builds on them from `main` now.
- ✅ The epic no longer lists federation and the stream epic under
  `Required` (rejected: keeping them). An epic's `Required` holds back every
  child, so certificate credentials and the Endpoint waited on federation and
  on JS streams they do not use. The relay node and the Endpoint each list
  their own blocker.
- ✅ The relay node takes a credential handle and the application enrolls
  however it likes (rejected: the coordinator issuing certificates;
  deferring). `moq_auth::mesh`'s token enrollment stays one way to get a
  handle. [Mesh enrollment can grow](/quest/m2/api-mesh-enrollment.md)
  fences the reply before the cut publishes it.
- ✅ The contract changes land in m2, because the cut publishes
  `moq_auth::mesh` for the first time: the
  grant shape (landed) and the
  [key budget](/quest/m2/mesh-device-bounds.md).
- The JS enrollment client and the guide are parked in m5: the application
  they were planned for enrolls with certificates and documents itself.

## Related

- [Outbound federation](/quest/m3/cluster-routing/federation.md) - the relay node requires it; the other children do not
- [Rust byte streams](/quest/m1/stream/rust.md) - the Endpoint requires it; the other children do not
- [P2P](/quest/m4/p2p/README.md) - internet peer sessions, which the m3 mesh does not use
- [Mesh over ICE](/quest/m5/mesh-ice.md) - deferred direct paths between mesh nodes on different networks
- [End-to-end encryption](/quest/m5/e2ee/README.md) - the AEAD the encryption level will reuse
- [Teleoperation](/quest/m5/teleop-robot.md) - robot track conventions that could ride the mesh
