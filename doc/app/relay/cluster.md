---
title: Clustering
description: Run multiple moq-relay instances across multiple hosts/regions
---

# Clustering

Multiple relay instances can cluster for geographic distribution and improved latency.

## Overview

`moq-relay` uses a simple clustering scheme:

1. **Root node** - A single relay (can serve public traffic) that tracks cluster membership
2. **Other nodes** - Accept internet traffic and consult the root for routing

When a relay publishes a broadcast, it advertises its `node` address to other relays via the root.

## Configuration

```toml
[cluster]
root = "https://root-relay.example.com"  # Root node
node = "https://us-east.relay.example.com"  # This node's address
```

### Cluster Arguments

- `--cluster-root <HOST>` - Hostname/IP of the root node (omit to make this node the root)
- `--cluster-node <HOST>` - Hostname/IP of this instance (needs valid TLS cert)

## How It Works

1. Each relay connects to the root node on startup
2. When a publisher connects to any relay, that relay announces the broadcast
3. The root node tracks which relay has which broadcasts
4. When a subscriber connects, the relay queries the root to find the broadcast
5. Relays connect to each other to forward traffic

## Benefits

- **Lower latency** - Users connect to nearest relay
- **Higher availability** - Redundancy across regions
- **Geographic distribution** - Serve global audiences

## Example Topology

```text
                    ┌─────────────┐
                    │  Root Node  │
                    │   (US-C)    │
                    └──────┬──────┘
           ┌───────────────┼───────────────┐
           │               │               │
    ┌──────┴──────┐ ┌──────┴──────┐ ┌──────┴──────┐
    │   US-East   │ │   EU-West   │ │   Asia-SE   │
    │   Relay     │ │   Relay     │ │   Relay     │
    └─────────────┘ └─────────────┘ └─────────────┘
```

## Current Limitations

- **Mesh topology** - All relays connect to all others
- **Not optimized for large clusters** - 3-5 nodes recommended
- **Single root node** - Future: multi-root for redundancy

## Production Example

The public CDN at `cdn.moq.dev` uses this clustering approach:

- `usc.cdn.moq.dev` - US Central (root)
- `euc.cdn.moq.dev` - EU Central
- `sea.cdn.moq.dev` - Southeast Asia

Clients use GeoDNS to connect to the nearest relay automatically.

## Next Steps

- Deploy to [Production](/app/relay/prod)
- Set up [Authentication](/app/relay/auth)
- Learn about [Protocol concepts](/concept/layer/)
