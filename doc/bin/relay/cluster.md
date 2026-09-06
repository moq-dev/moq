---
title: Clustering
description: Connect relays across hosts and regions
---

# Clustering

Relays connect to each other and forward announcements and subscriptions. A
viewer talks to the nearest relay; if the broadcast lives elsewhere, that relay
pulls it from a peer and caches it, so the second viewer in a region costs no
upstream bandwidth.

Each broadcast carries the list of relays it passed through. That hop list
catches loops and picks the shortest route, and every relay breaks ties the
same way so the cluster converges instead of flapping. Both wire protocols
carry it: natively on moq-lite, and via the [cluster extension](/draft/moq-cluster)
on moq-transport 17+.

## Topology

List the peers each relay dials. That's the whole topology.

```toml
# us-west.toml
[cluster]
connect = ["https://us-east.example.com/"]
```

A chain (`eu-west <- us-east <- us-west`) dedupes fetches through the middle;
a full mesh trades that for one fewer hop. Mix shapes as your traffic demands.

## Link costs

Add `?cost=N` to a peer URL to route by price instead of hop count. An unpriced
link costs 1. A relay already carrying a broadcast re-announces it at cost 0,
so siblings pull the warm copy over the free intra-datacenter link instead of
paying for a second metered fetch. Standby publishers (a transcoder pool) can
advertise a high cost and drop it once they're working.

```toml
[cluster]
connect = ["https://sibling.same-dc/?cost=0", "https://us-east.example.com/?cost=10"]
```

## Discovery

Instead of listing every peer, tell each relay its own URL and turn on gossip.
Connected relays learn about each other and dial back; between any two
gossiping nodes, only the one with the smaller URL dials.

```toml
[cluster]
connect = ["https://us-east.example.com/"]
node = "https://us-west.example.com/"
mesh = true
```

A relay with `node` and `mesh` but no `connect` is a passive rendezvous.

## Dynamic peer lists

Point `connect_api` at an HTTP(S) endpoint or local file returning a JSON
array of peer URLs. The relay re-checks it (honoring `Cache-Control`, or
watching the file) and reconciles: new peers are dialed, missing ones dropped,
changed URLs redialed. A bad fetch keeps the last good list.

```toml
[cluster]
connect_api = "https://api.example.com/cluster/peers"
node = "https://us-west.example.com/"
```

## Identity

Set `cluster.id` to a stable non-zero integer so a restarted relay keeps its
place in hop lists instead of looking like a new node. Keep it below 2^53 if
browser clients decode it.

## Authentication

Peers authenticate with **mTLS** (recommended: `server.tls.root` on the
listener, `client.tls.cert`/`key` on the dialer) or a **JWT** (inline
`?jwt=` on a peer URL, or a shared `cluster.token` file for gossip). Dials
retry forever with capped backoff, so a rejected token is loud in the logs
rather than fatal. See [Authentication](/bin/relay/auth#mtls).

The `/nodes` [internal endpoint](/bin/relay/http#get-nodes) shows the cluster
as this relay sees it.
