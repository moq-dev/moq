# Mesh

## Goal

Install `moq mesh` on any Linux or macOS host or robot, enroll it into a mesh
served by a relay (self-hosted, or a hosted service implementing the same
contract), and reach any other node by its Ed25519 endpoint ID or MagicDNS name
for SSH, TCP forwards, and agents. Nodes on one LAN talk directly; everything
else goes through relays. Content is signed by default.

Non-goals: internet P2P or NAT traversal (remote traffic always crosses a
relay), TUN devices or IP routing, encryption (parked in
[mesh keys](/quest/m3/mesh-keys.md)), a member list that a compromised
coordinator cannot extend, and Windows.

## Plan

Decided 2026-10-25 with the maintainer (planning interview):

- The first shippable outcome is two hosts, one on the LAN and one remote, each
  running `moq mesh up`, with `ssh` to the other by ID; the LAN path wins when
  both share a network. DNS, agents, and the UI follow.
- Traffic is app-level [byte streams](/quest/m2/stream/README.md) and named
  tracks, never IP packets.
- The coordinator is generic and lives here: enrollment issues a relay URL and
  a token, and self-hosters run it with `moq auth`. A hosted service implements
  the same contract with its own accounts and billing.
- Direct paths only inside the LAN, through the existing `[cluster.lan]` mDNS
  discovery. No ICE, STUN, or hole punching; the [P2P](/quest/m2/p2p/README.md)
  line is separate.
- An endpoint ID is a persistent Ed25519 device key, iroh-style. It is not the
  [routing identity](/quest/m1/cluster-routing/routes.md), which each
  process declares fresh.
- Addressing is announcement: each node publishes under `mesh/<id>/...`, and
  reaching an ID is subscribing there. There is no separate directory.
- The token is the binding: enrollment proves the device key, then the
  coordinator issues a grant to publish `mesh/<id>/**` and subscribe
  `mesh/**`. Relays need no new check. Membership is the coordinator's token
  only; a Tailnet-Lock analog was declined.
- Protection is per broadcast, with levels that nest: identity (the token
  only), authentication (per-frame Ed25519 signatures, the default), and later
  encryption (signed plus moq-e2ee, waiting on [mesh keys](/quest/m3/mesh-keys.md)).
- The daemon is a `moq mesh` subcommand of the existing `moq` CLI, which
  already runs as a relay node.
- Services are opt-in and named; nothing is exposed by default and sshd's own
  authentication still applies.
- Robots are a primary use case: aarch64 builds, headless enrollment, and
  flapping uplinks are acceptance criteria, not follow-ups.
- Upstream exposes an iroh-shaped Rust API (not wire-compatible with iroh), and
  the daemon is built on it.
- It builds on [outbound federation](/quest/m1/cluster-routing/federation.md):
  the local relay dials its relays as `upstream` links and ranks LAN peers
  above them.

This epic owns the end-to-end test: two daemons on separate network namespaces
sharing a LAN segment, plus a third behind a relay only, enrolled through
`moq auth`, with SSH by ID taking the LAN path between the first two and the
relay path to the third, surviving a relay restart.

## Required

- [Outbound federation](/quest/m1/cluster-routing/federation.md) - a local relay dials its relays and prefers LAN peers
- [Byte streams](/quest/m2/stream/README.md) - lossless TCP-level streams over MoQ
- [Enrollment](/quest/m2/mesh/enroll.md) - a device proves its key and gets a relay URL and a scoped token, by device grant or a pre-authorized key
- [Endpoint API](/quest/m2/mesh/endpoint.md) - an iroh-shaped `Endpoint` that connects to an endpoint ID and service and accepts byte streams
- [Daemon](/quest/m2/mesh/daemon.md) - `moq mesh up`, `serve`, `pipe`, and `install`, with SSH by ID on two hosts
- [Robots](/quest/m2/mesh/robot.md) - aarch64 builds and a robot-shaped node surviving a flapping uplink
- [MagicDNS](/quest/m2/mesh/dns.md) - `ssh host.mesh` and any TCP client reach a peer's services by name
- [Agents](/quest/m2/mesh/agent.md) - agents publish under their endpoint and an MCP server messages any agent by ID
- [Mesh UI](/quest/m2/mesh/ui.md) - a `.status` track per node and a web component the daemon serves on localhost
- [Mesh guide](/quest/m2/mesh/guide.md) - the doc.moq.dev page for installing and running a mesh

## Related

- [P2P](/quest/m2/p2p/README.md) - internet peer sessions, which the mesh does not use
- [End-to-end encryption](/quest/m2/e2ee/README.md) - the AEAD the encryption level will reuse
- [Teleoperation](/quest/m3/teleop-robot.md) - robot track shapes that could ride the mesh
