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

moq-transport drafts 14 and 15 cannot ask for every broadcast, since an empty
namespace prefix is illegal before draft 16. On such a link to a peer without
the MoQ Solicit extension, such as moxygen, the relay does not ask for the
whole namespace and learns only what the peer announces unasked. moxygen
announces nothing unasked on draft 14, so that link discovers no broadcasts.

When a moq-lite-04 or later peer withdraws its last advertisement for a broadcast, a relay
drops every other route to it that passed through that peer, since each was
relayed from what the peer just withdrew, rather than falling back to them one
by one. During reconnect, another session from that peer can still advertise
the broadcast; an old session's withdrawal does not invalidate that route.

A relay two hops from the publisher's still holds routes relayed through others.
So a change of a broadcast's best route waits 300 ms before it is announced,
while a new broadcast and a removed one go out at once. By then the withdrawal
has usually removed the other stale routes too, and the relay sends one
retraction instead of advertising each stale path in turn. Requests still
follow the current best route immediately; only the announcement waits.

Routes carrying the same [publisher epoch](/concept/moq-lite#publisher-epochs)
serve one broadcast. When the route serving it dies, withdraws, or is beaten by
a cheaper route with that epoch, each subscription continues on the new route
from the first frame its readers lack, so they see
every frame once, mid-group included. A route that is still up finishes the
groups it has open, overlapping the new one. A group neither route delivers is
dropped once the readers' max delay has passed it. A route through the subscribing peer
itself is never used. A publisher whose groups restart, such as an encoder
restarting from group 0, publishes under a new epoch, which replaces the old
broadcast instead of resuming it. RTMP, SRT, and WHIP ingest mint one per
connection, so an encoder reconnecting to the same path replaces its stale
connection at once. Epochs order by their creation time, so a reconnect to a
different gateway assumes the two gateways' clocks roughly agree. A route without an epoch, including every
route on moq-lite 06 and older or moq-transport, keeps its subscriptions until it
goes. Seamless failover needs both: a publisher that announces an epoch, and
moq-lite 07 on every link the route crosses, since epochs travel on nothing
older. A cluster that mixes moq-lite 06 and 07 links to the same content has a
second cost: the route that carries the epoch supersedes the one that does not
each time it appears, so a flapping moq-lite 07 link cuts the viewers resolved
through the older one.

Failover routes must carry copies of the same broadcast. For each track, the
relay requires matching timescale, retention window, publisher priority, and
group ordering. A source with different properties is refused before it serves
the track. If no compatible source remains, the track fails with
`Unsupported`. New immutable properties require a new track name or broadcast
name.

A route whose original publisher (its first hop) changes is updated in place on
both wire protocols, so the broadcast never briefly vanishes downstream, and
subscriptions in flight carry on through it.

## Topology

List the peers each relay dials. That's the whole topology: a relay dials only
peers from `connect`, [`connect_api`](#dynamic-peer-lists), or
[LAN discovery](#lan-discovery), never a URL learned from an announcement.

```toml
# us-west.toml
[cluster]
connect = ["https://us-east.example.com/"]
```

A chain (`eu-west <- us-east <- us-west`) dedupes fetches through the middle;
a full mesh trades that for one fewer hop. Mix shapes as your traffic demands.

For a full mesh, list every other relay, or serve the list from `connect_api`.
A session carries both directions, so one dial per pair is enough; listing a
pair on both sides opens a redundant second session.

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

The mark belongs to the link, whichever side dialed. A peer this relay dials
is upstream when its `connect` or `connect_api` entry says so. A peer that
dials in is upstream when its [grant](/bin/relay/auth#the-contract) sets
`"upstream": true` beside `"peer": true`, so an edge whose auth server grants
that to core certificates treats a core that dials it as upstream too.
`moq auth serve --mtls-peer --mtls-upstream` grants it to every certificate,
so use it only where nothing but cores dial in with mTLS: on a hub that
leaves dial into, it marks every leaf upstream, and the hub stops forwarding
between them. A relay that predates the mark treats every link as transit, so a cluster
migrates one region at a time.

Which relay dials which is still the peer list's job: there are no roles and
no topology check, so whatever generates the list applies the layout's rules.
Hiding a core from clients is admission, not routing: its auth refuses any
session that is not a cluster peer.

## TLS links

A link inside a region is rarely congested, so it can skip QUIC: a `tls://`
peer URL dials qmux over TLS on TCP, verified with the same `connect.tls`
settings as any other dial. The accepting relay serves it from its TCP
listener with TLS on. That listener asks for no client certificate, so a peer
on it authenticates with a token (`cluster.token`, `token`, or `?jwt=`), not
mTLS.

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

Prefix advertisements are forwarded and costed the same way as an exact-path
route: each hop appends its identity, adds the link price, and passes the
claim on. An advertised prefix must overlap the publisher's grant, or it is
refused. A prefix wider than the grant is accepted, but it only routes requests
for paths the grant covers.

Routing prefers the longest covering prefix, then a fully identified hop list
over one that holds a 0 (an anonymous hop) at any depth, then the lowest cost,
then the shortest hop list, then a hash of the requested path and the hop list,
breaking any remaining tie toward the newest announcement so a reconnecting
publisher isn't outranked by the session it replaced. Hashing the requested
path spreads equal-cost advertisers of one prefix, such as a transcode pool,
across its paths instead of sending every path to one of them, and every relay
that holds the same routes picks the same one for a given path. An assigned
identity for an anonymous peer is local selection state and is never written
into the hop list.

```toml
[cluster]
connect = ["https://sibling.same-dc/?cost=0", "https://us-east.example.com/?cost=10"]
```

The same policy reads as an object, which is the only form that accepts
`egress`, `token`, and [`upstream`](#upstream-links). A bare URL stays valid, and an object whose `url` still
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
# secret = "/etc/moq/cluster.key"     # Optional. 64 hex chars, or a file holding them.
# app = "default"                     # DNS-SD subtype; moq-cli shares this name.
```

A LAN peer authenticates with its mDNS credential on `/.cluster/<credential>`
and is never handed `cluster.token`; that token is for `connect` and
`connect_api` peers only. The advertisement carries the listener fingerprint
when the certificate was generated or supplied in-memory, the `node` URL when
one is configured, and at least one of them. `secret` is optional. Without it, anyone who can
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

## Failure detection

A peer that crashes or drops off the network sends no goodbye, so a relay only
learns it is gone when the link goes quiet for [`quic.idle_timeout`](/bin/relay/config#quic)
(10s by default). Until then its routes stay in place and subscribes through
them go nowhere. Lower it to fail over faster; raise it if a lossy long-haul
link drops while the peer is still alive, and keep `quic.keep_alive` well under
it.

QUIC uses the smaller of the two endpoints' idle timeouts
([RFC 9000 section 10.1](https://www.rfc-editor.org/rfc/rfc9000#section-10.1)),
so either relay on a link can shorten it for both. iroh links use the same
timeout; WebSocket links keep their own 30s deadline.

## Authentication

Peers dial with **mTLS** (recommended: `listen.tls.root` on the listener,
`connect.tls.cert`/`key` on the dialer) or a **JWT** (inline `?jwt=` on a peer
URL, `token` on a peer object, or a shared `cluster.token` file for every
listed peer). The
accepting relay admits a peer through the same lease as any client: its
certificate is reported to the auth server, which grants it, so a mesh needs
`moq auth serve --mtls-publish '**' --mtls-subscribe '**' --mtls-peer` (or a
server of your own that grants the cluster CA with `peer: true`) behind
`--auth-url`. A relay on `--auth-public '**'` admits peers through that grant
instead, as long as they send no `cluster.token`: public rules refuse a token.
Such a peer is admitted as a client, so what it forwards counts as ingested
here. LAN peers
authenticate with the mDNS credential on `/.cluster/<credential>`, a secret
the relay minted for itself and checks locally, and never receive
`cluster.token`. Dials retry forever with capped backoff, so a rejected peer
is loud in the logs rather than fatal. See [Authentication](/bin/relay/auth#mtls).

A relay records whether each route entered here or came from a peer, which the
hop list alone cannot say: a client and a peer each add one hop. Routes over a
dial this relay made, and over an accepted LAN peer, count as a peer's. An
accepted peer counts only when its grant sets `peer: true`, as `--mtls-peer`
does; otherwise it looks like a client ingesting here. An embedder reads this
as `Route::source()` and filters with `origin::Consumer::local()`.

The `/nodes` [internal endpoint](/bin/relay/http#get-nodes) lists the peers
this relay dialed and holds a session with.
