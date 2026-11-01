# [M] Address hints

## Goal

Mesh nodes that can reach each other directly but cannot discover each other
over mDNS still connect directly: each node publishes its dialable addresses
(interface addresses, its listen port, its certificate fingerprint), signed by
its endpoint key, on a track under `mesh/<id>/`, and every node dials peers'
hinted addresses alongside `[cluster.lan]`. Two daemons on a network namespace
with multicast disabled (the stand-in for a Docker bridge or a cloud VPC) find
the direct path and prefer it over the relay.

## Plan

Decided 2026-10-31 (maintainer, moq.pro mesh planning interview): mDNS fails
in Docker bridge and overlay networks, cloud VPCs, across subnets and VLANs,
on isolated Wi-Fi, and for macOS launchd agents denied Local Network access.
Hints cover the cases where peers can reach each other. Generic: the
coordinator adds nothing, and self-hosted meshes get it.

Decided 2026-10-31 (maintainer): the daemon is the relay, and the rule in
`doc/bin/relay/cluster.md` ("never a URL learned from an announcement") does
not apply when the operator opts in to the mesh and everything is signed.
Dial a hinted address only when the mesh is enabled, the hint verifies against
the endpoint key named by its `mesh/<id>/` path, and the dial pins the hinted
fingerprint; admit the peer only after it proves its own enrollment, as LAN
peers do. Amend that sentence of the docs with the mesh exception.

- Settle the track name and schema and record them here; keep it separate
  from `.status` so a node can read hints without the UI's payload.
- Publish every non-loopback interface address, public ones included: a host
  with a public address is directly dialable from anywhere, with no punching.
  A private address that collides with an unrelated host on another network
  fails the fingerprint pin, so it costs one failed dial, not a wrong peer.
- Bound and back off failed dials per peer, so a fleet of unreachable hints
  does not become a dial storm; record the bound chosen.
- The [sparse overlay](/quest/m2/mesh/overlay.md) decides which reachable
  peers to keep; until it lands, dial every hinted peer.

Tests: namespaces with multicast off take the direct path; a forged hint
(signed by another key) is never dialed; a colliding private address fails the
pin and falls back to the relay; a peer that stops answering is backed off.
Update `doc/bin/relay/cluster.md` and the [guide](/quest/m2/mesh/guide.md)
if it exists.

## Required

- [Daemon](/quest/m2/mesh/daemon.md) - the relay that publishes and dials the hints
