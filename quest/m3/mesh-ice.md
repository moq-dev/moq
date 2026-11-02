# [L] Mesh over ICE

## Goal

Mesh nodes on different networks that cannot dial each other's addresses
connect directly through the [P2P](/quest/m2/p2p/README.md) line's native
QUIC-over-ICE pairs, with the relay as rendezvous and fallback, and the
direct link joins the [sparse overlay](/quest/m2/mesh/overlay.md) as an
ordinary link.

## Plan

Parked 2026-10-31 (maintainer, moq.pro mesh planning interview): remote mesh
traffic is meant to cross relays, so hole punching waits in m3 until a mesh
user needs it. Reuse the P2P line's ICE, signaling under the mesh's own
prefix, and operator-supplied ICE servers; no TURN, since the relay already
carries the fallback route.

## Required

- [Native pairs over ICE and QUIC](/quest/m2/p2p/native-quic.md) - the transport this reuses
- [Sparse overlay](/quest/m2/mesh/overlay.md) - the overlay the direct link joins
