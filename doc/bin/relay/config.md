---
title: Configuration
description: TOML configuration reference for moq-relay
---

# Configuration

moq-relay is configured via a TOML file. Pass the path as the only argument:

```bash
moq-relay relay.toml
# or
moq-relay --config relay.toml
```

## Minimal Example

```toml
[server]
listen = "0.0.0.0:4443"

[server.tls]
cert = "cert.pem"
key = "key.pem"
```

## Full Reference

### \[log]

Logging configuration.

```toml
[log]
# Log level: trace, debug, info, warn, error
# The RUST_LOG environment variable takes precedence
level = "info"
```

### \[server]

QUIC/WebTransport server settings.

```toml
[server]
# Listen address for QUIC (UDP)
listen = "0.0.0.0:4443"
```

### \[server.tls]

TLS configuration for the QUIC endpoint.

```toml
[server.tls]
# Option 1: Provide certificate files
cert = "/path/to/cert.pem"   # Certificate chain
key = "/path/to/key.pem"     # Private key

# Option 2: Generate self-signed certificates (development only)
generate = ["localhost", "127.0.0.1"]

# Optional: root CAs to accept for mTLS peer authentication.
# Clients that present a cert signed by one of these CAs are granted
# full access (publish/subscribe/cluster). Intended for relay clustering.
# Quinn backend only.
root = ["/path/to/peer-ca.pem"]
```

For production, use certificates from Let's Encrypt or another CA.

### \[web.http]

HTTP server for debugging endpoints.

```toml
[web.http]
# Listen address for HTTP (TCP)
# Defaults to disabled if not specified
listen = "0.0.0.0:4443"
```

See [HTTP Endpoints](/bin/relay/http) for available endpoints.

### \[web.https]

HTTPS/WSS server for TCP fallback.

```toml
[web.https]
# Listen address for HTTPS/WSS (TCP)
listen = "0.0.0.0:443"

# TLS certificates (can be the same as server.tls)
cert = "cert.pem"
key = "key.pem"
```

### \[auth]

Authentication configuration.

```toml
[auth]
# Path to the JWT verification key
# - Symmetric: the shared secret key
# - Asymmetric: the public key
key = "root.jwk"

# Path prefix for anonymous access
# Omit to require authentication everywhere
public = "anon"
```

See [Authentication](/bin/relay/auth) for details on token generation.

### \[cluster]

Clustering configuration for multi-relay deployments.

```toml
[cluster]
# Address of the root relay to connect to
# Omit this to make this relay the root
connect = "root.relay.example.com:4443"

# JWT token file for cluster authentication
token = "cluster.jwt"

# This relay's address, as reachable by other cluster nodes
node = "leaf1.relay.example.com:4443"
```

See [Clustering](/bin/relay/cluster) for deployment patterns.

### \[client]

Client settings used when connecting to other relays (clustering).

```toml
[client]
# Disable TLS verification (development only!)
tls.disable_verify = true

# Or provide trusted root certificates
# tls.root = ["/path/to/root.pem"]
```

### \[stats]

Per-node stats publishing. When enabled, the relay publishes a single
`<prefix>/node/<node>` broadcast (or `<prefix>/node` when `node` is unset)
carrying JSON snapshots of every broadcast it's currently serving.

```toml
[stats]
# Master switch (defaults to false)
enabled = true

# Top-level path under which stats broadcasts are published (defaults to ".stats")
prefix = ".stats"

# Tick interval in seconds between snapshots (defaults to 1)
tick_secs = 1

# Number of ticks an idle broadcast lingers in the emitted frame after its
# last observed active subscription (defaults to 10). A short reconnect
# window keeps the entry visible across brief disconnects.
retention_ticks = 10

# Node identifier appended to the advertised path to disambiguate broadcasts
# when multiple relays share a cluster origin. May be multi-segment, e.g.
# "sjc/1" / "sjc/2" for two hosts nested under a shared region key.
# Single-relay deployments can omit this.
node = "sjc/1"
```

Each stats broadcast carries four tracks, one per `(tier, role)` pair:

| Track                       | What it covers                              |
|-----------------------------|---------------------------------------------|
| `publisher.json`            | external (e.g. customer) egress             |
| `subscriber.json`           | external ingress                            |
| `internal/publisher.json`   | internal (e.g. mTLS cluster peer) egress    |
| `internal/subscriber.json`  | internal ingress                            |

Each frame is a JSON object mapping broadcast path to a cumulative
counter snapshot. A broadcast appears in the frame while it has at least one
active subscription on that `(tier, role)` slot, and lingers for
`retention_ticks` ticks after the last one drops:

```json
{
  "demo/bbb": { "broadcasts": 1, "broadcasts_closed": 0, "subscriptions": 5,
                "subscriptions_closed": 2, "bytes": 12345, "frames": 678, "groups": 9 },
  "anon/foo": { "broadcasts": 1, "broadcasts_closed": 0, "subscriptions": 2,
                "subscriptions_closed": 0, "bytes": 234,   "frames": 12,  "groups": 1 }
}
```

Tier, role, and node are implied by the track and broadcast paths, so they
aren't repeated inside the frame. Counters are cumulative; a downstream
aggregator computes rates from successive snapshots. Frames for any one
`(tier, role)` are skipped when the JSON is byte-identical to the last
emitted frame, so idle periods don't burn bandwidth.

Every flag also accepts an equivalent CLI argument (`--stats-enabled`,
`--stats-prefix`, `--stats-tick-secs`, `--stats-retention-ticks`,
`--stats-node`) and environment variable (`MOQ_STATS_ENABLED`,
`MOQ_STATS_PREFIX`, `MOQ_STATS_TICK_SECS`, `MOQ_STATS_RETENTION_TICKS`,
`MOQ_STATS_NODE`).

### \[iroh]

Experimental P2P support via iroh.

```toml
[iroh]
# Enable iroh for P2P connections
enabled = false

# Path to persist the iroh secret key
secret = "./relay-iroh-secret.key"
```

## Example Configurations

See the [`demo/relay/`](https://github.com/moq-dev/moq/tree/main/demo/relay) directory for working configuration files:

- **Development** - [`demo/relay/root.toml`](https://github.com/moq-dev/moq/blob/main/demo/relay/root.toml)
- **Production** - [`demo/relay/prod.toml`](https://github.com/moq-dev/moq/blob/main/demo/relay/prod.toml)
- **Cluster Leaf Node** - [`demo/relay/leaf0.toml`](https://github.com/moq-dev/moq/blob/main/demo/relay/leaf0.toml)

## Environment Variables

- `RUST_LOG` - Override the log level (e.g., `RUST_LOG=debug`)
- `MOQ_IROH_SECRET` - Set the iroh secret key directly

## See Also

- [Authentication](/bin/relay/auth) - JWT setup
- [HTTP Endpoints](/bin/relay/http) - Debug endpoints
- [Clustering](/bin/relay/cluster) - Multi-relay deployments
- [Production Deployment](/setup/prod) - Production checklist
