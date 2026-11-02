# [L] Mesh UI

## Goal

Each node, a daemon or an application embedding `Endpoint`, publishes
`mesh/<id>/.status`: its peers, links (each with its smoothed and minimum
RTT, and for a relay uplink the relay's identity), routes, and services,
signed. A web component subscribes to it and draws the mesh as a graph: nodes,
links labeled with RTT, and the routed path between two selected nodes with
its summed RTT. `moq mesh ui` opens it at a localhost page the daemon serves,
showing that node's view, and a hosted service embeds the same component to
show a whole mesh through its relays, adding nodes and links of its own (such
as its PoPs) through a documented extension point.

## Plan

Decided 2026-10-25 (maintainer): the data is a track on `mesh/<id>`, not an
HTTP API; the daemon serves the page and hands it the WebTransport certificate
hash, because browsers block public pages from reaching localhost. Ship the
component as a package from `js/` so a hosted dashboard uses the same code.

Decided 2026-10-31 (maintainer, moq.pro mesh planning interview): per-link
RTTs and a graph view, and embedded apps publish `.status` so they are graph
nodes. RTT comes from the session's existing connection stats; coalesce
updates to a few a minute. moq.pro adds its PoPs and their inter-PoP RTTs
through the extension point, so settle its shape (extra nodes and links with
labels) and record it here.

## Required

- [Daemon](/quest/m2/mesh/daemon.md) - the node whose state the track carries
