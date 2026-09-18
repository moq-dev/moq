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

Every link has a price, and the cheapest path wins. By default the price is
measured: each relay samples the cluster sessions it sends on once a second and
prices a link by what it does to a live stream, its round-trip time, one more
round trip for the share of groups a lost packet stalls, and a penalty for
terminating QUIC on one more relay:

```
cost_ms = rtt * (1 + stall) + hop_penalty
stall   = 1 - (1 - loss) ^ (group_bytes / 1200)
```

A lost packet costs about a round trip to recover and holds every frame behind
it in its group, so what loss does to a stream depends on how many packets a
group spans: 1% loss stalls nearly every 300 KB video group and one in
twenty-five 4 KB audio groups. A link that has carried no group yet is priced
against the largest group the relay carries on its other links. The sender prices the link because only it sees
its own loss and the groups it sends, and it folds the price into every route
it forwards, so nothing new crosses the wire. RTT is a median over the last 15
samples; loss and the group size are ratios over the last 5000 packets sent,
however long ago, so an idle link keeps what its last stream measured. Prices
round to `step` and only move once they have drifted a whole step, and a route
only moves to a path that beats the one in use by more than `hop_penalty`, so a
path does not flap on one slow ack or on several links drifting at once. Costs ride moq-lite-06 and the MoQ Cluster extension; a link
negotiated on an older version carries hop counts only, whatever it measures.

Loss is only learned from packets, so a link that has carried nothing is priced
on its RTT until a stream crosses it, and the first stream pays to discover a
lossy edge. `probe` buys that knowledge up front: each relay asks its peers to
pad every measured link up to that many bits per second whenever nothing else
flows (the PROBE `Increase` level, moq-lite-06), so a 1% edge shows up within a
minute at 100 kbit/s for about 12 KB/s per idle link.

```toml
[cluster.cost]
hop_penalty = "8ms"       # What one more relay costs a stream. Default.
step = "5ms"              # Prices round to this and move by whole steps. Default.
# probe = 100000          # Bits per second of padding on every idle link. Off by default.
# measure = false         # Price every unpriced link at 1 instead (hop counting).
```

A relay prices its links once it is part of a cluster: it dials peers, gossips,
joins a LAN, or, for a relay that only accepts peers, has a `node` URL of its
own. A pricing relay publishes its links under `.internal/links/<hop id>` as a
JSON track, and reads its peers' tables to name, at `info`, every direct link
that a two-hop path through another peer beats by more than the hop penalty.
Routing takes such a detour on its own; the log is what tells an operator which
backbone edges are worth having. A pricing relay says so in its SETUP, so a peer
charges nothing more for the link on arrival; a peer on a release without that
flag still adds its default of 1, and a relay that measures nothing (`measure =
false`, or an older release) is priced at 1 per link by its peers, which ranks
its links as nearly free next to measured ones.

To price a link by hand instead, add `?cost=N` to the peer URL. A configured
price is the operator's policy for that link, so both ends keep it and neither
measures it. Each relay adds the price of the link an announcement arrived on
before forwarding it, so a route's cost is the sum of what it crossed.

Wildcard advertisements are forwarded and costed the same way as an exact-path
route: each hop appends its identity, adds the link price, and passes the
claim on. An advertisement must be contained by one of the publisher's granted
prefixes (`grant/**`); an over-wide pattern is refused rather than clamped.

Routing prefers the most specific pattern, then a fully identified hop list
over one that holds a 0 (an anonymous hop) at any depth, then the lowest cost,
then the shortest hop list, breaking any remaining tie toward the newest
announcement so a reconnecting publisher isn't outranked by the session it
replaced. An assigned identity for an anonymous peer is local selection state
and is never written into the hop list. Resolving a non-prefix pattern into a
subscription is not implemented yet.

```toml
[cluster]
connect = ["https://sibling.same-dc/?cost=0", "https://us-east.example.com/?cost=10"]
```

The same policy reads as an object, which is the only form that accepts
`egress` and `token`. A bare URL stays valid, and an object whose `url` still
carries `?cost=` or `?jwt=` alongside those fields is rejected rather than
given a precedence a migration could silently get wrong.

```toml
[cluster]
connect = [
  { url = "https://sibling.same-dc/", cost = 0 },
  { url = "https://us-east.example.com/", cost = 10, egress = 10, token = "PEER_JWT" },
]
```

`cost` is what this relay charges to pull from the peer. `egress` is what it
declares in SETUP as its own price toward the peer and defaults to `cost`;
anything else is refused until asymmetric routing lands, so the two are always
equal today. `token` replaces an inline `?jwt=` with identical authorization
and redaction.

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
was generated or supplied in-memory, the `node` URL when one is configured,
and at least one of them. `secret` is optional. Without it, anyone who can
reach the listener joins, so leave it unset only on networks you trust. With
it, only peers that prove they hold the same key are discovered or accepted.
mDNS is still an open channel: the secret authenticates the record, it does
not hide the credential or the node URL. `app` names the DNS-SD application
this relay advertises under; peers using a different name never discover it.
It defaults to `default`, which moq-cli shares so the two find each other
with no configuration. An application built on the library picks its own
name. Startup waits for at least one interface to announce before the relay
reports itself ready.

## Dynamic peer lists

Point `connect_api` at an HTTP(S) endpoint or local file returning a JSON
array of peers: bare URL strings and/or the same objects `connect` accepts.
The relay re-checks it (honoring `Cache-Control`, or
watching the file) and reconciles: new peers are dialed, missing ones dropped,
changed URLs redialed. A bad fetch keeps the last good list.

```json
["https://a.pop.example/?cost=1", {"url": "https://b.pop.example/", "cost": 2}]
```

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

Peers dial with **mTLS** (recommended: `listen.tls.root` on the listener,
`connect.tls.cert`/`key` on the dialer) or a **JWT** (inline `?jwt=` on a peer
URL, `token` on a peer object, or a shared `cluster.token` file for static and
gossip peers). The
accepting relay admits a peer through the same lease as any client: its
certificate is reported to the auth server, which grants it, so a mesh needs
`moq auth serve --mtls-publish '**' --mtls-subscribe '**'` (or a server of
your own that grants the cluster CA) behind `--auth-url`. A relay on
`--auth-public '**'` admits peers through that grant instead. LAN peers
authenticate with the mDNS credential on `/.cluster/<credential>`, a secret
the relay minted for itself and checks locally, and never receive
`cluster.token`. Dials retry forever with capped backoff, so a rejected peer
is loud in the logs rather than fatal. See [Authentication](/bin/relay/auth#mtls).

The `/nodes` [internal endpoint](/bin/relay/http#get-nodes) shows the cluster
as this relay sees it.
