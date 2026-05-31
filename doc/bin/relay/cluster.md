---
title: Clustering
description: Run multiple moq-relay instances across multiple hosts/regions
---

# Clustering

Relays can be joined together to proxy announcements and subscriptions between each other. A viewer talks to whichever relay is closest; if their broadcast lives somewhere else in the cluster, the local relay fetches it from a neighbor and caches it.

A broadcast carries a small hop list as it travels. Each relay it passes through adds itself to the list, which is how loops are caught and how the network picks the shortest path when there's more than one. When two paths are the same length, every relay breaks the tie the same way (a hash of the broadcast name and hop list), so the whole cluster converges on one route instead of flapping between equals.

## Topology

Each relay lists the peers it wants to dial in `cluster.connect`. That's it; the topology is whatever you draw with those links.

A simple chain works well when one region is the source and others are caches:

```text
eu-west  <---  us-east  <---  us-west
```

```toml
# us-east.toml
[cluster]
connect = ["eu-west.example.com:4443"]

# us-west.toml
[cluster]
connect = ["us-east.example.com:4443"]
```

A publisher on `eu-west` reaches a viewer on `us-west` through `us-east`. If a second `us-west` viewer subscribes to the same broadcast, `us-east` already has it cached, so only one fetch crosses the Atlantic. A full mesh (every relay dialing every other) would skip the cache entirely and waste an outbound link per pair.

Pick the shape that matches your traffic. Linear chains are great for fanout; small N-way meshes are fine when latency matters more than dedup; mixed shapes work too.

## Auto-discovery

Listing every peer by hand can get tedious in larger clusters. Tell the relay its own URL with `cluster.node`, then enable gossip with `cluster.mesh`; connected peers will discover and dial it back automatically:

```toml
[cluster]
connect = ["us-east.example.com:4443"]
node    = "us-west.example.com:4443"
mesh    = true
```

`node` is this relay's identity (its externally-reachable URL); `mesh` is a boolean that turns gossip on. Each gossiping node creates a broadcast carrying its `node` address, which other nodes pick up. `connect` is optional once gossip is running, but you still need at least one connection somewhere (either you dial a peer or a peer dials you) for the advertisement to flow. Enabling `mesh` without `node` is an error, since there'd be no address to advertise.

When two gossiping nodes discover each other, only one of them dials: the node with the lexicographically-smaller URL is the client, the larger is the server. The session is bidirectional, so a single connection carries announcements both ways and the pair avoids opening two redundant links. This tiebreaker applies only to gossip-discovered peers; an explicit `connect` entry always dials.

A relay with `node` + `mesh` and no `connect` is a passive rendezvous: it sits and waits for inbound connections, then helps everyone else find each other.

## Dynamic peer lists

`cluster.connect` is fixed at startup, so adding or removing a node means editing every affected config and restarting. When you'd rather keep the topology somewhere external and change it without a redeploy, point `cluster.connect_api` at an HTTP(S) endpoint or a local file:

```toml
[cluster]
connect_api = "https://api.example.com/cluster/connect"
node        = "us-west.example.com:4443"
```

The source returns a bare JSON array of peer hostnames:

```json
["eu-west.example.com:4443", "us-east.example.com:4443"]
```

The relay reconciles that list against its live dials: new entries are dialed, entries that disappear are dropped. It composes with `connect` (static seeds that are never reconciled away) and `mesh` (gossip). The relay's own `node` value, when set, is sent as a `?node=` query parameter so the endpoint can return the peers for that specific node; for mTLS-gated endpoints the cluster client certificate identifies the caller as well.

- **HTTP(S) URL**: polled, with the re-poll cadence taken from the response's `Cache-Control` reuse window (`max-age` plus any `stale-while-revalidate`; default 30s, floored at 5s). Conditional revalidation (`ETag` / `Last-Modified`) and stale-if-error (serving the last cached body when a revalidation request fails) are handled by the underlying HTTP cache, so transient endpoint blips don't churn the dial set.
- **Local file** (a path or `file://` URL): watched via OS filesystem notifications (inotify / FSEvents / kqueue), with a periodic re-check as a safety net.

If a fetch fails or returns garbage, the relay logs and keeps the last good list rather than tearing the cluster down. This keeps the moq-relay binary generic: all routing decisions (which node connects where) live in whatever service answers the endpoint.

## Authentication

Cluster peers must authenticate to each other:

- **mTLS** (recommended). Set `tls.root` to the CA that signed the cluster certificates. Inbound connections presenting a valid client cert are granted full access; outbound dials use `client.tls.cert` / `client.tls.key`.
- **JWT**. Each relay reads a token from `cluster.token` and presents it on outbound dials. The token needs broad enough scope to cover whatever paths the cluster carries.

See [Authentication](/bin/relay/auth) for the full setup.

## Migration from older configs

`cluster.root` was removed; setting it errors at startup with a message pointing at the replacement. `cluster.mesh` is now a boolean gossip toggle (it used to take this relay's URL); the URL moved to `cluster.node`. The old `mesh = "<url>"` form still works for backwards compatibility: it enables gossip and is treated as `cluster.node`, with a deprecation warning (or an error if it conflicts with an explicit `cluster.node`).

| Old | New |
|---|---|
| `root = "rendezvous:4443"` + `node = "us-east:4443"` | `connect = ["rendezvous:4443"]` + `node = "us-east:4443"` + `mesh = true` |
| `root = "rendezvous:4443"` only | `node = "rendezvous:4443"` + `mesh = true` (passive rendezvous) |
| `mesh = "us-east:4443"` | `node = "us-east:4443"` + `mesh = true` |

## Next steps

- Deploy to [Production](/bin/relay/prod)
- Set up [Authentication](/bin/relay/auth)
- Learn about [Protocol concepts](/concept/layer/)
