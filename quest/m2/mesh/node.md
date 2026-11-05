# [M] Mesh relay node

## Goal

A relay node (the `moq-relay` library, and its binary through a `[mesh]`
config section) joins a mesh: it enrolls with an enrollment key through
`moq_auth::mesh::Client`, signing with its endpoint key, renews before the
token expires, dials the mesh's relays as `upstream` links, and admits a
`[cluster.lan]` peer only after the peer proves its own enrollment. A restart
keeps the endpoint key and enrollment, so a node never re-enrolls on its
own. A mesh daemon outside this repository (Kixel's) embeds this rather than
reimplementing it.

## Plan

Decided 2026-11-02 (maintainer, Kixel planning): moq keeps the libraries and
Kixel the binary. This is the library half of the deleted daemon quest; its
CLI verbs, service install, and UI moved to Kixel. Carried over from it:

- Mesh relays are `upstream` links; LAN peers come from `[cluster.lan]` mDNS
  and rank above them, with no extra shared secret to configure.
- Enroll and renew with `moq_auth::mesh::Client`; the contract is in
  `doc/bin/relay/auth.md#mesh-enrollment`. Pass the hostname as the default
  name.
- Nothing is served unless the embedding application names it.

Tests in network namespaces: two relay nodes enroll through `moq auth`, take
the LAN link when they share a segment and the relay path otherwise, refuse
a LAN peer without an enrollment, and survive a relay restart. Update
`doc/bin/relay/cluster.md` inline.

## Required

- [Outbound federation](/quest/m1/cluster-routing/federation.md) - upstream links and LAN preference
