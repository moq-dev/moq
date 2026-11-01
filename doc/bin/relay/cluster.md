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
on moq-transport.

A track from a moq-transport publisher keeps its Object IDs, object properties,
and END\_OF\_GROUP status objects across a moq-lite-07 link, since relays
always offer the [ietf extension](/draft/moq-ietf) that carries them. Over an
older moq-lite link they are lost: a moq-transport subscriber behind it sees
consecutive objects with no properties.

When a moq-lite-04 or later peer withdraws a broadcast, the routes relayed
through it go with it at once. A change of best route is announced after
300 ms, so stale paths are retracted once instead of advertised in turn;
requests follow the new best route immediately. For 2 s after a relay retracts
a broadcast, it holds back a worse path to the same publisher that a peer still
hunting offers, instead of announcing the broadcast again.

Routes carrying the same [publisher epoch](/concept/moq-lite#publisher-epochs)
serve one broadcast. When the route serving it dies, withdraws, or is beaten by
a cheaper one with that epoch, each subscription continues on the new route
from the first frame its readers lack, so they see every frame once. A
publisher whose groups restart, such as an encoder restarting from group 0,
publishes under a new epoch, which replaces the old broadcast instead of
resuming it; the RTMP, SRT, and WHIP ingests mint one per connection. Epochs
order by creation time, so a reconnect to a different gateway assumes the two
gateways' clocks roughly agree. Seamless failover needs a publisher that
announces an epoch and, on every link the route crosses, moq-lite 07 or
moq-transport with the [epoch extension](/draft/moq-epoch). A route
without an epoch, including every route on moq-lite 06 and older or a
moq-transport link without the extension, keeps its subscriptions until it goes or a track fails
upstream, and any other route winning it is announced as a restart. Viewers
re-request on a restart, and a relay drops its copy of the old instance,
including every path beneath a prefix claim when one of them moves to another
worker.

Failover routes must carry copies of one publisher's broadcast. Independent
importers use distinct epochs and switch as a restart, even when fed one stream.
See [Redundant ingest](/setup/redundancy) to choose between publisher switching,
subscriber selection, and several routes to one publisher.
A source whose track differs in any property, such as timescale, retention,
publisher priority, compression, or refresh, is refused for that track. A
moq-transport link records its draft in the track's properties and carries
refresh only as on or off, so a route crossing one does not fail over to or from
a route that does not.

A route whose original publisher (its first hop) changes under the same epoch is
updated in place on both wire protocols, so the broadcast never briefly vanishes
downstream, and subscriptions in flight carry on through it. Without an epoch
that is another source, so it restarts.

## Topology

List the peers each relay dials. That's the whole topology: a relay dials only
peers from `connect`, [`api.url`](#dynamic-peer-lists), or
[LAN discovery](#lan-discovery), never a URL learned from an announcement.

```toml
# us-west.toml
[cluster]
connect = ["https://us-east.example.com/"]
```

A chain (`eu-west <- us-east <- us-west`) dedupes fetches through the middle;
a full mesh trades that for one fewer hop. Mix shapes as your traffic demands.

For a full mesh, list every other relay, or serve the list from `api.url`.
A session carries both directions, so one dial per pair is enough; listing a
pair on both sides opens a redundant second session.

`cluster.max_hops` bounds the hop path this relay accepts or forwards, from 1
to 32, with 32 as the default. Set it near the mesh's diameter after any single
link or node failure, plus a margin, to cut off long detours during withdrawal
path hunting. Count the publisher's hop too. A cap below that diameter can
partition the mesh; a lower cap only drops longer paths and preserves route
selection among the remaining paths. A peer hears the best route with room
for this relay's hop, even when a cheaper route at the cap serves locally.

```toml
[cluster]
max_hops = 6
```

The same setting is `--cluster-max-hops` or `MOQ_CLUSTER_MAX_HOPS`. It covers
moq-lite-03 through moq-lite-07-wip and moq-transport drafts 18 through 22 when
the MoQ Cluster extension is negotiated. A forwarded path counts this relay's
own hop even when its peer appends it on receipt. Older versions and peers
without hop paths hide their depth, which this setting cannot bound.

## Upstream links

Mark a link `upstream` and this relay never offers it a route learned on
another upstream link. Everything else still transits, and clients still see
every route the relay knows. An edge marks its region's cores upstream, so it
reaches every core and every core reaches its clients, but it never carries
traffic from one core to another. A drone marks its CDN uplink upstream, so its
mesh reaches the CDN and the CDN reaches the mesh, but the drone never carries
traffic between CDN relays.

```toml
# edge.toml
[cluster]
connect = [
  { url = "https://core-a.example.com/", upstream = true },
  { url = "https://core-b.example.com/", upstream = true },
]
```

The mark belongs to the link, whichever side dialed: a dialed peer is upstream
when its `connect` entry says so, and a peer that dials in when its
[grant](/bin/relay/auth#the-contract) sets `"upstream": true` beside
`"peer": true`. `moq auth serve --mtls-link upstream` grants that to
every certificate, so use it only where cores are the only relays dialing in
with mTLS: on a hub that leaf relays dial into, it marks every leaf upstream
and the hub stops forwarding between them. A relay that predates the mark treats every link as
transit, so a cluster migrates one region at a time. There are no roles and no
topology check; whatever generates the peer list applies the layout's rules.

## TLS links

A link inside a region is rarely congested, so it can skip QUIC: a `tls://`
peer URL dials qmux over TLS on TCP, verified with the same `connect.tls`
settings as any other dial. The accepting relay serves it from its TCP
listener with TLS on. That listener applies the same client-certificate policy
as the QUIC listener, so a peer on it authenticates with its `connect.tls`
certificate against `listen.tls.root`, or with a token (`cluster.token`,
`token`, or `?jwt=`).

```toml
# core.toml
[listen.tcp]
bind = "[::]:4443"
tls = true

# edge.toml
[cluster]
connect = [{ url = "tls://core-a.internal:4443/", upstream = true }]
```

## Link costs

Add `?cost=N` to a peer URL to route by price instead of hop count. An unpriced
link costs 1, which reproduces plain hop counting. Each relay adds the price of
the link an announcement arrived on before forwarding it, so a route's cost is
the sum of what it crossed.

Routing prefers the longest covering prefix, then a fully identified hop list
over one holding an anonymous hop (0), then the lowest cost, then the shortest
hop list. Remaining ties hash the requested path, so equal-cost advertisers of
one prefix, such as a transcode pool, split its paths, and every relay picks
the same one for a given path. A publisher connected straight to the relay is
not hashed at its own path: its newest session wins, so a publisher that
restarts takes over from its lingering old session at once.

```toml
[cluster]
connect = ["https://sibling.same-dc/?cost=0", "https://us-east.example.com/?cost=10"]
```

The same policy reads as an object, the only form that accepts `token` and
[`upstream`](#upstream-links):

```toml
[cluster]
connect = [
  { url = "https://sibling.same-dc/", cost = 0 },
  { url = "https://us-east.example.com/", cost = 10, token = "PEER_JWT" },
]
```

Prices aren't static. A publisher can re-price a live announcement, which is
how a standby transcoder pool seeds a high cost and drops it once it's working,
and a relay receiving a GOAWAY re-prices every route learned from that peer to
the maximum so new subscriptions go elsewhere while existing ones finish. A
relay keeps its session to a peer draining to restart until that peer closes
it, and redials only then, so it reaches the replacement rather than the
draining process.

A route's price is the publisher's production cost plus the link costs it
crosses. A live publisher seeds 0, and caching never changes it.

## LAN discovery

On a LAN there may be no one to list. `[cluster.lan]` advertises this relay
over mDNS and dials the peers that advertise back, so a rack or a home lab
meshes with no seed list. A `moq --cluster-lan` process on the same network
joins the same mesh:

```toml
[cluster]
# Optional. Without it the relay advertises its listen port and fingerprint,
# the way `moq --cluster-lan` does.
node = "us-west.local:4443"

[cluster.lan]
enabled = true
# secret = "/etc/moq/cluster.key"     # Optional. A file holding 64 hex chars (openssl rand -hex 32).
# app = "default"                     # DNS-SD subtype; moq-cli shares this name.
```

A LAN peer authenticates with a credential carried in its mDNS record and is
never handed `cluster.token`. Without `secret`, anyone who can reach the
listener joins, so leave it unset only on networks you trust. With it, only
peers that hold the same key are discovered or accepted. The secret
authenticates the record; it does not hide the node URL. Peers advertising a
different `app` never discover each other.

## Dynamic peer lists

Point `api.url` at an HTTP(S) endpoint or local file returning a JSON
array of peers: bare URL strings and/or the same objects `connect` accepts.
The relay re-checks it (honoring `Cache-Control`, or
watching the file) and reconciles: new peers are dialed, missing ones dropped,
changed URLs redialed. A bad fetch keeps the last good list.

```json
["https://a.pop.example/?cost=1", {"url": "https://b.pop.example/", "cost": 2}]
```

```toml
[cluster]
node = "https://us-west.example.com/"

[cluster.api]
url = "https://api.example.com/cluster/peers"
```

HTTPS peer-list fetches present the same client certificate as cluster dials,
but authenticate the API with system roots and the hostname in its URL.
Mesh roots, fingerprints, insecure mode, and the mesh hostname override do not
apply to the API. For a private API, set `cluster.api.tls.root` to PEM
paths (or repeat `--cluster-api-tls-root`); these replace system trust
for API fetches only. Malformed roots fail startup, and files reload for new
connections when rotated.

Remote API URLs must use HTTPS; plaintext HTTP is accepted only for loopback
hosts. Redirects are refused: configure the final API URL, since its peer list
decides which relays receive the cluster token.

## Identity

Each relay has a Hop ID: the value it adds to a route's hop list for loop
detection and shortest-path routing. It is random on every start, which is fine
for loop detection but makes a restarted relay look like a new node. Set
`cluster.id` to a stable non-zero 64-bit integer to pin it: below 2^53 if older
browser clients decode it, and below 2^62 to keep loop detection over moq-lite-06
and older links, which carry a larger id as 0.

## Failure detection

A peer that crashes or drops off the network sends no goodbye, so a relay only
learns it is gone when the link goes quiet for [`quic.idle_timeout`](/bin/relay/config#quic)
(10s by default). Until then its routes stay in place and subscribes through
them go nowhere. Lower it to fail over faster; raise it if a lossy long-haul
link drops while the peer is still alive. The keep-alive derives from it, so
leave `quic.keep_alive` unset.

QUIC uses the smaller of the two endpoints' idle timeouts
([RFC 9000 section 10.1](https://www.rfc-editor.org/rfc/rfc9000#section-10.1)),
so either relay on a link can shorten it for both. iroh links use the same
timeout; WebSocket links keep their own 30s deadline.

## Authentication

Peers dial with **mTLS** (recommended: `listen.tls.root` on the listener,
`connect.tls.cert`/`key` on the dialer) or a **JWT** (inline `?jwt=` on a peer
URL, `token` on a peer object, or a shared `cluster.token` file for every
listed peer). The accepting relay admits a peer like any client, so a mesh
needs an auth server that grants the cluster CA, such as
`moq auth serve --mtls-publish '**' --mtls-subscribe '**' --mtls-link peer`. An
accepted peer counts as a cluster peer only when its grant sets `peer: true`,
as `--mtls-link peer` does; otherwise what it announces counts as ingest here, like
a client's, and it lists [hidden](/concept/moq-lite#hidden-broadcasts) routes
only by asking, which a relay below moq-lite-07-wip, or on moq-transport
without the hidden extension, cannot. Dials retry forever with jittered backoff
capped at 30s, so a rejected peer is loud in the logs rather than fatal. See
[Authentication](/bin/relay/auth#mtls).

The `/nodes` [internal endpoint](/bin/relay/http#get-nodes) lists the peers
this relay dialed and holds a session with.
