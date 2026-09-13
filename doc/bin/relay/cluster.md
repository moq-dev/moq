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

Failover routes must carry copies of the same broadcast. For each track, the
relay requires matching timescale, retention window, publisher priority, and
group ordering. A source with different properties is refused before its groups
are spliced in. If no compatible source remains, the track fails with
`Unsupported`. New immutable properties require a new track name or broadcast
identity.

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
link costs 1, which reproduces plain hop counting. Each relay adds the price of
the link an announcement arrived on before forwarding it, so a route's cost is
the sum of what it crossed. Routing prefers the most specific announced prefix,
then the lowest cost, then the shortest hop list, breaking any remaining tie
toward the newest announcement so a reconnecting publisher isn't outranked by
the session it replaced.

```toml
[cluster]
connect = ["https://sibling.same-dc/?cost=0", "https://us-east.example.com/?cost=10"]
```

Price is per direction: pulling from a metered origin can cost far more than
pushing to it, so each end declares its own and the two need not match. Prices
aren't static either. A publisher can re-price a live announcement, which is how
a standby transcoder pool seeds a high cost and drops it once it's working, and
a relay receiving a GOAWAY re-prices every route learned from that peer to the
maximum so new subscriptions go elsewhere while existing ones finish.

moq-lite-06 announcements carry two prices, *warm* and *cold*. Both accumulate
identically today, so routing runs on link costs alone; the split reserves room
for a warm-copy discount, letting a relay advertise its cached copy cheaper on
the warm side while the cold price still says who sits closest to the publisher.
moq-transport has nowhere to carry the cold price, so a route learned from it
ranks with an unknown (worst-case) one.

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

On a LAN there may be no seed peer to gossip through. `[cluster.lan]` advertises
this relay over mDNS and dials the peers that advertise back, so a rack or a
home lab meshes with no seed list. A `moq --cluster-lan` process on the same
network joins the same mesh:

```toml
[cluster]
# Optional. Without it the relay advertises its listen port and fingerprint,
# the way `moq --cluster-lan` does.
node = "us-west.local:4443"

[cluster.lan]
enabled = true
# secret = "/etc/moq/cluster.key"     # Optional. 64 hex chars, or a file holding them.
# app = "default"                     # DNS-SD subtype; moq-cli shares this name.
```

A LAN peer authenticates with its mDNS credential on `/.cluster/<credential>`
and is never handed `cluster.token`; that token is for static and gossip peers
only. The advertisement carries the listener fingerprint when the certificate
was generated, the `node` URL when one is configured, and at least one of them.
`secret` is optional. Without it, anyone who can reach the listener joins, so
leave it unset only on networks you trust. With it, only peers that prove they
hold the same key are discovered or accepted. `app` names the DNS-SD
application this relay advertises under; peers using a different name never
discover it. It defaults to `default`, which moq-cli shares so the two find
each other with no configuration. An application built on the library picks
its own name. Startup waits for at least one interface to announce before the
relay reports itself ready.

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

Each relay has a Hop ID: the value it adds to a route's hop list for loop
detection and shortest-path routing. It is random on every start, which is fine
for loop detection but makes a restarted relay look like a new node. Set
`cluster.id` to a stable non-zero integer to pin it, below 2^53 if browser
clients decode it.

## Authentication

Peers authenticate with **mTLS** (recommended: `listen.tls.root` on the
listener, `connect.tls.cert`/`key` on the dialer) or a **JWT** (inline
`?jwt=` on a peer URL, or a shared `cluster.token` file for static and gossip
peers). LAN peers authenticate with the mDNS credential on
`/.cluster/<credential>` and never receive `cluster.token`. Dials retry
forever with capped backoff, so a rejected token is loud in the logs rather
than fatal. See [Authentication](/bin/relay/auth#mtls).

The `/nodes` [internal endpoint](/bin/relay/http#get-nodes) shows the cluster
as this relay sees it.
