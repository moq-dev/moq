---
title: Configuration
description: TOML configuration reference for moq-relay
---

# Configuration

moq-relay is configured via a TOML file. Pass the path as the only positional argument:

```bash
moq-relay relay.toml
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
# Supported by the quinn and noq backends.
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

### \[internal]

Unauthenticated qmux listeners (no TLS) for trusted local workers. Every
connection is granted full, unrestricted access. A TCP and a Unix-socket
listener can each be enabled independently; both default to disabled.

```toml
# Billing tier these sessions record stats under (default "internal"; empty = the
# default unprefixed tier). See the Stats section.
tier = "internal"

# Plain-TCP listener (tcp:// scheme).
[internal.tcp]
listen = "127.0.0.1:4444"

# Unix-socket listener (unix:// scheme), requires the `uds` build feature.
[internal.uds]
listen = "/run/moq/internal.sock"

# Restrict the Unix-socket callers by peer credentials. Empty/omitted = no check.
[internal.uds.allow]
uid = [1001]
# gid = [2000]
# pid = [12345]
```

A non-loopback TCP bind logs a warning but is allowed. See
[Internal Listener](/bin/relay/auth#internal-listener) for details.

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
# Peers this relay dials, as full URLs. The topology is whatever you draw with
# these links. A JWT may be supplied inline as a ?jwt= query parameter. A bare
# host or "host:port" is deprecated but still accepted (wrapped in https://.../).
connect = ["https://us-east.example.com/?jwt=..."]

# Optional. This relay's own externally-reachable URL (identity). Advertised to
# peers when gossip is on, and sent to connect_api as ?node=.
node = "us-west.example.com:4443"

# Optional. Enable gossip discovery: advertise `node` so peers find you
# automatically. Boolean; requires `node` to be set.
mesh = true

# Optional. Fetch the peer list from an HTTP(S) endpoint or local file (a JSON
# array of hostnames) and reconcile it at runtime, no restart needed.
connect_api = "https://api.example.com/cluster/connect"

# JWT for outbound cluster dials (alternative to mTLS), applied to any peer
# whose URL has no inline ?jwt=. Required to authenticate gossip / connect_api
# discovered peers; for static `connect` peers, prefer an inline ?jwt=.
token = "cluster.jwt"
```

See [Clustering](/bin/relay/cluster) for topology choices and the trade-off between hand-listed peers and gossip.

### \[client]

Client settings used when connecting to other relays (clustering).

```toml
[client]
# Disable TLS verification (development only!)
tls.disable_verify = true

# Or provide trusted root certificates. By default these replace the system
# roots, so the relay trusts only these CAs.
# tls.root = ["/path/to/root.pem"]

# Set this to also trust the platform's system roots alongside any custom root,
# e.g. to dial a local relay with a private CA and a remote one with a public CA.
# Defaults to true only when no custom root is set.
# tls.system_roots = true
```

### \[stats]

Per-node stats publishing. When enabled, the relay publishes a single
`<prefix>/node/<node>` broadcast (or `<prefix>/node` when `node` is unset)
carrying JSON snapshots of the broadcasts it's currently serving and of the
sessions currently connected to it.

```toml
[stats]
# Master switch (defaults to false)
enabled = true

# Top-level path under which stats broadcasts are published (defaults to ".stats")
prefix = ".stats"

# Seconds between snapshot publishes (defaults to 1)
interval = 1

# Node identifier appended to the advertised path to disambiguate broadcasts
# when multiple relays share a cluster origin. May be multi-segment, e.g.
# "sjc/1" / "sjc/2" for two hosts nested under a shared region key.
# Single-relay deployments can omit this.
node = "sjc/1"
```

Each stats broadcast splits traffic by **tier**, an arbitrary label chosen by
business logic (see the auth API's [`tier`](/bin/relay/auth#unified-auth-api-auth-api)
field). The default tier is unprefixed; a named tier prefixes its track names
with its label. So per tier the broadcast carries a publisher, a subscriber, and
a session track:

| Track                       | What it covers                              |
|-----------------------------|---------------------------------------------|
| `publisher.json`            | default-tier egress                         |
| `subscriber.json`           | default-tier ingress                        |
| `<tier>/publisher.json`    | named-tier egress (e.g. `internal/publisher.json`) |
| `<tier>/subscriber.json`   | named-tier ingress                          |
| `sessions.json`             | default-tier connected sessions, keyed by root |
| `<tier>/sessions.json`     | named-tier connected sessions, keyed by root |

The default-tier tracks always exist (emitting `{}` while idle). A named tier's
tracks are created the first time traffic routes to that label, so cluster
fan-out shows up under `internal/*` (the default trusted-peer label) and never in
the default-tier numbers.

Trusted (non-JWT/public) traffic defaults to the `internal` tier, configurable
per source: `--cluster-tier` (relay-to-relay dials), `--internal-tier` (the
`[internal]` listener), and `--auth-mtls-tier` (mTLS peers, when the auth API
doesn't return a `tier`). Set any of them to a different label, or to `""` to
record on the default unprefixed tier.

Each per-broadcast frame is a JSON object mapping broadcast path to a
cumulative counter snapshot. An entry surfaces on any tick where the
broadcast is live (any open counter still exceeds its `*_closed`
counterpart, so a subscription could begin at any moment) or its snapshot
changed since the previous tick. Once every counter equals its `*_closed`
counterpart no traffic can flow, so the entry is dropped:

```json
{
  "demo/bbb": {
    "announced": 1, "announced_closed": 0,
    "broadcasts": 1, "broadcasts_closed": 0,
    "subscriptions": 5, "subscriptions_closed": 2,
    "bytes": 12345, "frames": 678, "groups": 9
  },
  "anon/foo": {
    "announced": 1, "announced_closed": 0,
    "broadcasts": 1, "broadcasts_closed": 0,
    "subscriptions": 2, "subscriptions_closed": 0,
    "bytes": 234, "frames": 12, "groups": 1
  }
}
```

Field semantics:

- `announced` / `announced_closed`: cumulative count of every broadcast
  announce/unannounce event on this `(tier, role)` slot, regardless of
  whether any subscription happened. Use this for "all known broadcasts".
- `broadcasts` / `broadcasts_closed`: per-(broadcast, session)
  subscription sentinel. The first active subscription a peer session
  opens for a broadcast bumps `broadcasts`; the last one it closes bumps
  `broadcasts_closed`. Summed across sessions, `broadcasts -
  broadcasts_closed` is the number of distinct sessions currently
  subscribed to the broadcast (i.e. viewers on the egress side), which is
  typically what billing and UI want.
- `subscriptions` / `subscriptions_closed`: cumulative count of
  track-level subscription guards opened and dropped.
- `bytes` / `frames` / `groups`: cumulative payload counters from the
  session loops (both the `moq-lite` and IETF `moq-transport` paths).

The session tracks (`sessions.json` and any `<tier>/sessions.json`) instead map
each auth root to a `{ sessions, sessions_closed }` snapshot. `sessions`
bumps when a session authenticated under that root connects and
`sessions_closed` when it disconnects, so `sessions - sessions_closed` is
the number of sessions currently connected under the root. This counts
presence regardless of whether any data flows, so a client connected to
e.g. `/acme` is billable even while idle. A root entry is emitted while live
or on the tick it changed, then dropped once no session under it remains:

```json
{
  "acme":   { "sessions": 3, "sessions_closed": 1 },
  "globex": { "sessions": 1, "sessions_closed": 0 }
}
```

Tier, role, and node are implied by the track and broadcast paths, so
they aren't repeated inside the frame. Counters are cumulative and
strictly monotonic; a counter going *backwards* across successive
snapshots means the underlying entry was garbage-collected and
re-created (relay restart or a long idle gap). Downstream consumers
should treat decreases as a fresh session segment and sum across resets
when computing lifetime totals.

Each snapshot reads `*_closed` atomics before their open counterparts,
which guarantees the emitted snapshot never shows `closed > open` even
under concurrent bumps (it can momentarily show an inflated *open* count,
which is logically valid).

Frames for any one `(tier, role)` are skipped when the JSON is
byte-identical to the last emitted frame; new subscribers still pick up
a baseline immediately via track-latest semantics.

Every flag also accepts an equivalent CLI argument (`--stats-enabled`,
`--stats-prefix`, `--stats-interval`, `--stats-node`) and environment
variable (`MOQ_STATS_ENABLED`, `MOQ_STATS_PREFIX`, `MOQ_STATS_INTERVAL`,
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
