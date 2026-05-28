---
title: Clustering
description: Run multiple moq-relay instances across multiple hosts/regions
---

# Clustering

Relays can be joined together to proxy announcements and subscriptions between each other. A viewer talks to whichever relay is closest; if their broadcast lives somewhere else in the cluster, the local relay fetches it from a neighbor and caches it.

A broadcast carries a small hop list as it travels. Each relay it passes through adds itself to the list, which is how loops are caught and how the network picks the shortest path when there's more than one.

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

Listing every peer by hand can get tedious in larger clusters. Set `cluster.mesh` to this relay's own URL and connected peers will discover and dial it back automatically:

```toml
[cluster]
connect = ["us-east.example.com:4443"]
mesh    = "us-west.example.com:4443"
```

Each node with `mesh` set creates a broadcast carrying its address, which other nodes pick up. `connect` is optional once gossip is running, but you still need at least one connection somewhere (either you dial a peer or a peer dials you) for the advertisement to flow.

A relay with `mesh` set and no `connect` is a passive rendezvous: it sits and waits for inbound connections, then helps everyone else find each other.

## Authentication

Cluster peers must authenticate to each other:

- **mTLS** (recommended). Set `tls.root` to the CA that signed the cluster certificates. Inbound connections presenting a valid client cert are granted full access; outbound dials use `client.tls.cert` / `client.tls.key`.
- **JWT**. Each relay reads a token from `cluster.token` and presents it on outbound dials. The token needs broad enough scope to cover whatever paths the cluster carries.

See [Authentication](/bin/relay/auth) for the full setup.

## Migration from older configs

`cluster.root` and `cluster.node` were both removed. If a config still sets either flag, the relay errors at startup with a message pointing at the replacements:

| Old | New |
|---|---|
| `root = "rendezvous:4443"` + `node = "us-east:4443"` | `connect = ["rendezvous:4443"]` + `mesh = "us-east:4443"` |
| `root = "rendezvous:4443"` only | `mesh = "rendezvous:4443"` (passive rendezvous) |
| `node = "us-east:4443"` | `mesh = "us-east:4443"` |

## Next steps

- Deploy to [Production](/bin/relay/prod)
- Set up [Authentication](/bin/relay/auth)
- Learn about [Protocol concepts](/concept/layer/)
