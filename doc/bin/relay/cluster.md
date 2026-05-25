---
title: Clustering
description: Run multiple moq-relay instances across multiple hosts/regions
---

# Clustering

Multiple relay instances can join a cluster for geographic distribution and improved latency. Every cluster peer publishes into the same logical origin; loop detection and shortest-path preference come from a hop list on each broadcast, so peers can be connected in arbitrary topologies without duplicating data.

## Two ways to form a cluster

Pick the mode that matches your operational constraints. Both can be combined in a single deployment.

### Static topology

Enumerate every peer by URL. Best for small clusters (2-5 nodes) where membership rarely changes.

```toml
[cluster]
connect = ["us-east.example.com:4443", "eu-west.example.com:4443"]
```

Each entry in `connect` is dialed at startup and kept alive with exponential backoff. There is no discovery: every new node requires editing every existing config.

### Gossip discovery

Each relay sets `node` to its own externally-reachable URL. Connecting to a single peer is enough; that peer gossips the new node's address to everyone else.

```toml
# On the rendezvous (every other relay connects here)
[cluster]
node = "rendezvous.example.com:4443"

# On a leaf joining the cluster
[cluster]
node = "us-east.example.com:4443"
connect = ["rendezvous.example.com:4443"]
```

When a leaf with `node` set connects to `rendezvous`, it publishes a registration broadcast at `.internal/origins/<node>` on the cluster origin. Other peers reachable from `rendezvous` see the registration and dial the new leaf, building a full mesh. Removing a node unannounces its registration, which aborts the dial on every other peer.

A relay with `node` set and no `connect` entries waits passively for inbound connections. A relay with `connect` and no `node` dials peers but isn't itself advertised, so others won't discover it via gossip.

## How gossip works

1. On startup, a relay with `cluster.node = "<url>"` publishes a placeholder broadcast at `.internal/origins/<url>` on its own origin. The broadcast carries no tracks: the path is the registration.
2. Cluster sessions exchange their origins both ways. The registration propagates to every connected peer, accumulating a hop chain along the way.
3. Each peer watches `.internal/origins/*` for newly announced URLs and dials any it isn't already connected to. Dials are deduplicated by URL, so a peer reached via both `connect` and gossip uses a single session.
4. When a peer goes away, its registration is unannounced and every other relay aborts the dial it spawned in response.
5. Loop detection on `publish_broadcast` refuses any broadcast whose hop chain already contains this relay's id, so re-announcing a registration through a longer path is a silent no-op.

## Visibility of `.internal/*`

Mesh registrations are infrastructure, not user data. The relay restricts the `.internal/` namespace to internal sessions:

- **mTLS peers** (cluster-to-cluster traffic, authenticated against `tls.root`) see `.internal/*` and can publish into it. This is how registrations flow between relays.
- **JWT-authenticated sessions** are filtered: their subscribe view hides `.internal/*` announcements, and their publish view refuses publishes to `.internal/*`. This holds even for tokens with the broadest possible scope (`subscribe = [""]`, `publish = [""]`).
- **Anonymous sessions** under `auth.public` are bound by the configured public prefixes; `.internal/` is not one of them.

The split is enforced at session acceptance, so there is no way to reach `.internal/*` without first authenticating via a trusted client certificate.

## Peer authentication

Cluster peers must authenticate to each other before they exchange registrations. Two options:

### mTLS (recommended for new deployments)

Configure the relay with `tls.root` pointing at the CA that signed the cluster peer certificates. Inbound connections presenting a valid client cert are granted full access (`AuthToken::unrestricted`) and tagged as internal. Leaves connect outbound with a `client.tls.cert` / `client.tls.key` signed by the same CA. No JWT is required.

See [Authentication → mTLS Peer Authentication](/bin/relay/auth#mtls-peer-authentication) for the CA setup walkthrough.

### JWT token

Each relay reads a JWT from `cluster.token` and presents it on outbound dials. The token must grant full publish and subscribe scope (`publish: ""`, `subscribe: ""`). The receiving relay verifies it like any other JWT.

```toml
[cluster]
node = "us-east.example.com:4443"
connect = ["rendezvous.example.com:4443"]
token = "cluster.jwt"
```

JWT-authenticated cluster sessions are tagged as external for stats purposes. **`.internal/*` is mTLS-only**: a JWT session, no matter how broad its scope, is filtered out of `.internal/origins/*` and cannot publish or receive mesh registrations. JWT-only cluster peers can still relay user traffic for each other, but they will not participate in gossip discovery. Use mTLS for any deployment that wants peers to find each other automatically.

## Example topology (3-node gossip cluster)

```text
              ┌──────────────────────┐
              │  rendezvous.exam.com │
              │  cluster.node = ...  │
              └──┬──────────────┬────┘
                 │              │
       gossip ┌──┘              └──┐ gossip
              │                    │
   ┌──────────┴──────┐    ┌────────┴────────┐
   │ us-east.exam.com│◀──▶│ eu-west.exam.com│
   │  node + connect │    │  node + connect │
   └─────────────────┘    └─────────────────┘
                ▲    direct (gossip)    ▲
                └─────────────────────────┘
```

`us-east` and `eu-west` each set `connect = ["rendezvous.example.com:4443"]`. The rendezvous gossips them to each other; the resulting topology is a full mesh.

## Production example

The public CDN at `cdn.moq.dev` uses gossip-style discovery across regions:

- `usc.cdn.moq.dev` - US Central
- `euc.cdn.moq.dev` - EU Central
- `sea.cdn.moq.dev` - Southeast Asia

Clients use GeoDNS to connect to the nearest relay automatically.

## Migration from older configs

`cluster.root` was removed in favor of the gossip / static split. If a config still sets it (CLI flag `--cluster-root` or TOML `[cluster] root = "..."`), the relay errors at startup with a message pointing at `--cluster-connect` and `--cluster-node`. Two minimal migrations:

| Old (pre-rewrite) | New equivalent |
|---|---|
| `root = "rendezvous:4443"` + `node = "us-east:4443"` | `connect = ["rendezvous:4443"]` + `node = "us-east:4443"` |
| `root = "rendezvous:4443"` (root-only node) | `node = "rendezvous:4443"` (passive rendezvous) |

The `node` field on leaves keeps its meaning; only the entry-point flag was renamed from `root` to `connect`, and `connect` now accepts a list.

## Next steps

- Deploy to [Production](/bin/relay/prod)
- Set up [Authentication](/bin/relay/auth)
- Learn about [Protocol concepts](/concept/layer/)
