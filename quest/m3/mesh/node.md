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
- A LAN peer proves its enrollment with the coordinator token, or with a
  [certificate credential](/quest/m3/mesh/cert-credentials.md) when the
  embedding application supplies a trust store (decided 2026-11-07,
  maintainer).

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ the node
takes a credential handle, and the embedding application decides how it
enrolls (rejected: `moq_auth::mesh`'s coordinator and `Enrollment` learning
to issue certificates, which puts one consumer's contract in a public crate;
deferring). The handle yields what the node presents to the mesh's relays
and to LAN peers and says when to renew; `moq_auth::mesh::Client`'s token
enrollment is the implementation this repository ships and the `[mesh]`
config section uses, and a
[certificate credential](/quest/m3/mesh/cert-credentials.md) is another. The
Goal's "enrolls with an enrollment key" describes that default, not the only
path. Keep the handle a small trait or enum in moq-relay's mesh module, private
to the crate until an embedder outside this repository needs to implement it.

Tests in network namespaces: two relay nodes enroll through `moq auth`, take
the LAN link when they share a segment and the relay path otherwise, refuse
a LAN peer without an enrollment, and survive a relay restart. Update
`doc/bin/relay/cluster.md` inline.

## Required

- [Outbound federation](/quest/m3/cluster-routing/federation.md) - upstream links and LAN preference
