---
title: Clustering
description: Run multiple moq-relay instances across multiple hosts/regions
---

# Clustering

Relays can be joined together to proxy announcements and subscriptions between each other. A viewer talks to whichever relay is closest; if their broadcast lives somewhere else in the cluster, the local relay fetches it from a neighbor and caches it.

A broadcast carries a small hop list as it travels. Each relay it passes through adds itself to the list, which is how loops are caught and how the network picks the shortest path when there's more than one. When two paths are the same length, every relay breaks the tie the same way (a hash of the broadcast name and hop list), so the whole cluster converges on one route instead of flapping between equals.

Both wire protocols carry this. `moq-lite` has the hop list and route cost in its announcements natively; `moq-transport` gets them from the [MoQ Cluster extension](/draft/moq-cluster), negotiated per session on `moqt-17` and later. A peer that doesn't speak the extension still works, it just contributes no path or price of its own.

## Topology

Each relay lists the peers it wants to dial in `cluster.connect`. That's it; the topology is whatever you draw with those links. Each peer is a full URL (e.g. `https://us-east.example.com/`); a bare host or `host:port` is deprecated but still accepted, and is wrapped in `https://.../` with a warning.

A simple chain works well when one region is the source and others are caches:

```text
eu-west  <---  us-east  <---  us-west
```

```toml
# us-east.toml
[cluster]
connect = ["https://eu-west.example.com/"]

# us-west.toml
[cluster]
connect = ["https://us-east.example.com/"]
```

A publisher on `eu-west` reaches a viewer on `us-west` through `us-east`. If a second `us-west` viewer subscribes to the same broadcast, `us-east` already has it cached, so only one fetch crosses the Atlantic. A full mesh (every relay dialing every other) would skip the cache entirely and waste an outbound link per pair.

Pick the shape that matches your traffic. Linear chains are great for fanout; small N-way meshes are fine when latency matters more than dedup; mixed shapes work too.

## Link costs

Hop counting treats every link the same, but links rarely cost the same: traffic between two relays in one datacenter is free, while a metered backbone bills per byte. Announcements carry a route cost so relays can route by price instead of distance, on `moq-lite-06` (still work-in-progress and opt-in via `--version`) and on `moqt-17` and later.

Price a link by adding `?cost=N` to the peer URL:

```toml
[cluster]
connect = [
  "https://sibling.same-dc.example/?cost=0",
  "https://us-east.example.com/?cost=10",
]
```

`?cost=N` prices what *this* relay charges to pull a broadcast from that peer, so it steers this relay's own routing. It is also declared during setup, which tells the peer what pulling from us costs, so a link priced on one side alone still ranks the same from both. The param is consumed locally; it is never sent as part of the URL.

Price is per direction. Pulling from a metered origin can cost far more than pushing to it, so each end declares its own and the two need not match; a relay that receives a price it disagrees with keeps its own `?cost=` for its own routing. An unpriced direction costs 1, which reproduces plain hop counting.

The cost a relay advertises is the *marginal* cost of pulling the broadcast through it. A relay actively carrying a broadcast (a subscriber is pulling it) re-announces it at cost 0: its upstream fetch is already paid for, so a sibling should pull the warm copy over a free intra-DC link instead of opening a second metered fetch. When the last subscriber leaves, the cost decays back after a short grace period. Standby publishers (e.g. a transcoder pool) can seed a large cost so they are only selected when nothing cheaper exists, and the winner's cost drops to 0 once it starts working.

That discount is what makes the cluster share one copy, but it also erases something. Once several relays are all carrying the broadcast they *all* advertise 0, so asking "what does this cost you?" gets the same answer everywhere and can no longer say which of them ought to be the one doing the pulling.

So `moq-lite-06` announcements carry two prices instead of one:

- **warm**: what pulling costs right now, given which relays already have it. Zero at any relay that is carrying. This is the number routing minimizes, and it is what consolidates the fleet onto one copy.
- **cold**: what the same route would cost if no relay had it. It ignores the discounts entirely, so it does not collapse, and it keeps saying how far each relay sits from the publisher.

The first picks where a relay fetches from. The second picks which relay does the fetching: a carrying relay hands its subscribers over to another carrying relay only when that one's cold price is lower, so the relay nearest the publisher becomes the aggregation point and the rest consolidate onto it. Relays equally far from the publisher tie there and fall back to a hash of the broadcast path, which spreads ownership across the fleet instead of funneling every broadcast onto one of them.

Both matter. Without the warm price every relay opens its own fetch and the backbone carries N copies. Without the cold price the relays that are already carrying cannot be told apart, so the aggregation point lands wherever a coin flip puts it, which can leave a relay one cheap link from the publisher pulling through a relay several expensive links away.

A relay waits about half a second before moving onto another relay, and re-checks when the wait is up. Prices are reported, not observed, so a report still crossing the mesh can be cheaper than what its sender would say now, and while prices are rising a ring of relays could otherwise each defer to a stale neighbour and all let go at once, leaving nobody pulling. The wait outlasts that propagation, so the re-check runs on current prices, and it carries a small per-relay spread so a whole PoP does not reconsider on the same instant. Some moves never wait: reconnecting to the peer you were already pulling from (recognized by its declared identity, so a relay that withheld one waits like anything else), and repairs, meaning replacing a route that has vanished or stopped announcing, which would otherwise keep the relay on a dead source while a live one sits in the table. Leaving a *draining* route does wait, because a relay that received a GOAWAY keeps serving until its handover window closes, and a fleet draining together is exactly when relays re-parent off prices that have not landed. If the drain turns into a disconnection the route vanishes, and that case is immediate, so the wait is never longer than the session it is waiting on. Only trading a working upstream for a better one waits, and that includes a relay with no viewers, whose choice is simply the one it will pull down when a viewer arrives.

`moq-transport` has nowhere to carry the cold price, so those routes keep the older coin-flip behavior.

## Auto-discovery

Listing every peer by hand can get tedious in larger clusters. Tell the relay its own URL with `cluster.node`, then enable gossip with `cluster.mesh`; connected peers will discover and dial it back automatically:

```toml
[cluster]
connect = ["https://us-east.example.com/"]
node    = "us-west.example.com:4443"
mesh    = true
```

`node` is this relay's identity (its externally-reachable URL); `mesh` is a boolean that turns gossip on. Each gossiping node creates a broadcast carrying its `node` address, which other nodes pick up. `connect` is optional once gossip is running, but you still need at least one connection somewhere (either you dial a peer or a peer dials you) for the advertisement to flow. Enabling `mesh` without `node` is an error, since there'd be no address to advertise.

When two gossiping nodes discover each other, only one of them dials: the node with the lexicographically-smaller URL is the client, the larger is the server. The session is bidirectional, so a single connection carries announcements both ways and the pair avoids opening two redundant links. This tiebreaker applies only to gossip-discovered peers; an explicit `connect` entry always dials.

A relay with `node` + `mesh` and no `connect` is a passive rendezvous: it sits and waits for inbound connections, then helps everyone else find each other.

### On a local network

On a LAN there may be no seed peer to gossip through and no external service to
list peers. `cluster.lan` advertises this relay over mDNS and dials the peers
that advertise themselves back, so a rack or a home lab meshes with no seed list
at all:

```toml
[cluster]
node = "us-west.local:4443"

[cluster.lan]
enabled = true
secret  = "/etc/moq/cluster.key"
```

mDNS only replaces *how peers find each other*. They are dialed at their `node`
URL and authenticate with `cluster.token` exactly like a gossiped or
`connect_api` peer, the same tiebreaker decides which side dials, and the same
dial map means a peer found two ways still opens one session. `lan.enabled`
without `node` is an error, since there'd be no address to advertise.

`lan.secret` is required rather than optional, and every peer needs the same
value (`openssl rand -hex 32 > cluster.key`). mDNS is an open channel: anyone on
the network can advertise, including an attacker naming a URL it controls. Since
the relay attaches `cluster.token` to any peer it dials, an unauthenticated
advertisement would be enough to collect that token. So each advertisement
carries a proof of key possession, bound to the record it travels in so it
cannot be copied into someone else's, and a relay only dials peers whose proof
verifies.

Startup waits for the mesh to come up before the relay reports itself ready
(`READY=1` under systemd), so a bad key, a missing `node`, or a host that cannot
announce on any interface fails the relay rather than releasing the units that
depend on it. One working interface is enough: a down VPN adapter or a container
bridge with multicast off doesn't hold startup back. It only proves this host
can send, though, so see below for what it doesn't catch.

`lan` is for relays sharing a link, and only that. Peers off the network are
gossiped, listed by `connect_api`, or seeded in `connect`, and they reach each
other over their `node` URLs like any other cluster link. A relay is already the
public address both sides can reach, so there is nothing for peer-to-peer to
save there; on one link it saves the round trip out and back.

Both hosts need inbound packets for this to work, in two places. mDNS is inbound
multicast UDP on port 5353, and a host that blocks it still multicasts its own
announcements out, so peers discover it while it discovers nobody. The session
is then one dial per pair, taken by the side whose `node` URL sorts first, so the
*other* side is the one that has to accept an inbound connection on its `node`
port. Startup readiness only proves an interface announced, not that anything
answered, so a firewall shows up as a pair that never meshes rather than as a
relay that fails to start.

## Origin id

Each relay has an origin id: the value it adds to a broadcast's hop list for loop detection and shortest-path routing. On `moq-lite`, and on a `moqt-17`-or-later session that negotiated the cluster extension, each end declares it at setup so the other can avoid announcing (or serving) a path that already flows through it. Older sessions carry no identity, so a peer only has one if you assign it. By default a fresh random id is picked on every start, which is fine for loop detection but means a relay looks like a brand-new node each time it restarts.

Set `cluster.id` to pin a stable id across restarts:

```toml
[cluster]
id = 12345
```

The id must be non-zero and below 2^62 (the wire varint limit); an out-of-range value is an error at startup. Keep it below 2^53 if older `@moq/lite` browser clients connect to the cluster, since they decode hop ids as a `u53` and reject anything larger. Give each relay a distinct id, otherwise two nodes sharing one id can break loop detection.

## Dynamic peer lists

`cluster.connect` is fixed at startup, so adding or removing a node means editing every affected config and restarting. When you'd rather keep the topology somewhere external and change it without a redeploy, point `cluster.connect_api` at an HTTP(S) endpoint or a local file:

```toml
[cluster]
connect_api = "https://api.example.com/cluster/connect"
node        = "us-west.example.com:4443"
```

The source returns a JSON array of peer URLs. Legacy bare hosts remain accepted:

```json
["https://eu-west.example.com/?cost=10", "us-east.example.com:4443"]
```

The relay reconciles that list against its live dials: new entries are dialed, entries that disappear are dropped, and a changed URL for a `connect_api`-owned peer replaces its session. That includes dial-side inputs such as `?cost=` and an inline `?jwt=`. An identical render is a no-op. It composes with `connect` (static seeds that are never reconciled away) and `mesh` (gossip). If another source already owns a peer's session, the API entry remains its updated fallback until that source disappears. The relay's own `node` value, when set, is sent as a `?node=` query parameter so the endpoint can return the peers for that specific node; for mTLS-gated endpoints the cluster client certificate identifies the caller as well.

- **HTTP(S) URL**: re-checked every 30s, but freshness is delegated to a standard HTTP cache (`http-cache`), so the response's `Cache-Control` controls how often a check turns into a real fetch. While the cached list is still fresh (`max-age`), the re-check is served from cache with no network round-trip; once it's stale the cache issues a conditional GET (`ETag` / `Last-Modified`) and falls back to the last cached body if revalidation fails (stale-if-error). Set a longer `max-age` to reduce load on your endpoint, or `no-cache` to force a conditional GET on every tick. Transient endpoint blips don't churn the dial set.
- **Local file** (a path or `file://` URL): watched via OS filesystem notifications (inotify / FSEvents / kqueue), with a periodic re-check as a safety net.

If a fetch fails, an entry is invalid, or one identity has conflicting entries, the relay logs and keeps the entire last good list rather than applying a partial topology. This keeps the moq-relay binary generic: all routing decisions (which node connects where) live in whatever service answers the endpoint.

## Authentication

Cluster peers must authenticate to each other:

- **mTLS** (recommended). Set `tls.root` to the CA that signed the cluster certificates. Inbound connections presenting a valid client cert are granted full access; outbound dials use `connect.tls.cert` / `connect.tls.key`.
- **JWT**. Supply a per-peer token inline as a `?jwt=` query parameter on a static or `connect_api` URL. Alternatively, set `cluster.token` to a file holding the shared JWT; it is presented on any dial whose URL has no inline token. Gossip must use the shared token or mTLS: never put a JWT in `cluster.node`, because that URL is advertised to the mesh and written to logs. Either way the token needs broad enough scope to cover whatever paths the cluster carries.

See [Authentication](/bin/relay/auth) for the full setup.

Peers are redialed indefinitely, with exponential backoff and jitter so a restarting cluster doesn't
reconnect in lockstep. That includes a peer that rejects us: a bad token logs `cluster peer error;
will retry` on every attempt rather than giving up, so watch for a peer that never reaches
`cluster peer session closed`. The delay escalates to ten seconds at most, so a dead or rejecting
peer stays loudly visible in the logs and a returning one is picked up within seconds.

## Migration from older configs

`cluster.root` was removed. To dial cluster peers use `cluster.connect`; to advertise this relay's own address set `cluster.node` and enable `cluster.mesh`. `cluster.mesh` is now a boolean gossip toggle (it used to take this relay's URL); the URL moved to `cluster.node`. The old `mesh = "<url>"` form still works for backwards compatibility: it enables gossip and is treated as `cluster.node`, with a deprecation warning (or an error if it conflicts with an explicit `cluster.node`).

`cluster.connect` entries are now full URLs; a bare host or `host:port` still works but logs a deprecation warning. A per-peer JWT belongs inline as a `?jwt=` query parameter on a static or `connect_api` URL. The `cluster.token` file remains the shared fallback and is required for JWT-authenticated gossip; never put a JWT in the advertised `cluster.node` URL.

| Old | New |
|---|---|
| `root = "rendezvous:4443"` + `node = "us-east:4443"` | `connect = ["rendezvous:4443"]` + `node = "us-east:4443"` + `mesh = true` |
| `root = "rendezvous:4443"` only | `node = "rendezvous:4443"` + `mesh = true` (passive rendezvous) |
| `mesh = "us-east:4443"` | `node = "us-east:4443"` + `mesh = true` |
| `connect = ["host:4443"]` + `token = "c.jwt"` | `connect = ["https://host/?jwt=<token>"]` |

## Next steps

- Deploy to [Production](/bin/relay/prod)
- Set up [Authentication](/bin/relay/auth)
- Learn about [Protocol concepts](/concept/layer/)
