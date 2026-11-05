# [M] Status track

## Goal

Each mesh node, a relay node with the mesh enabled or an application
embedding `Endpoint`, publishes `mesh/<id>/.status`, signed: its peers, its
links (each with its smoothed and minimum RTT, and for a relay uplink the
relay's identity), its routes, and its services. A subscriber can draw the
whole mesh from these tracks alone.

## Plan

Decided 2026-10-25 (maintainer): the data is a track on `mesh/<id>`, not an
HTTP API.

Decided 2026-10-31 (maintainer, moq.pro mesh planning interview): per-link
RTTs, and embedded apps publish `.status` so they appear as nodes. RTT comes
from the session's existing connection stats; coalesce updates to a few a
minute.

Decided 2026-11-02 (maintainer, Kixel planning): the graph component, the
localhost page, and the hosted dashboard moved to Kixel, which draws them
from this track. Keep the schema generic and documented, with no extension
point for a hosting service's own nodes. Settle the schema and record it in
`doc/` with this quest.

Tests: two nodes in network namespaces publish `.status` naming each other's
link with an RTT, a relay uplink names its relay, and a frame signed by
another key is rejected by a subscriber.

## Required

- [Endpoint API](/quest/m2/mesh/endpoint.md) - the application node whose links the track describes
- [Mesh relay node](/quest/m2/mesh/node.md) - the relay node whose links the track describes
