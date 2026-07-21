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

QUIC/WebTransport server settings. Optionally add plaintext qmux stream
listeners for trusted local workers. Every connection authenticates through the
same JWT / public-access path; QUIC additionally accepts an mTLS client
certificate, and Unix sockets add optional peer-credential gating.

```toml
[server]
# QUIC (UDP) bind. Omit to run stream-only (no QUIC) when a tcp/unix listener
# is configured below.
bind = "[::]:443"

# Plaintext qmux over TCP (no TLS, carries no peer identity). Trusted networks
# only; a non-loopback bind logs a warning. Requires the `tcp` build feature.
[server.tcp]
bind = "127.0.0.1:4444"

# Plaintext qmux over a Unix socket, for local workers (e.g. the protocol
# gateways or a stats publisher). Requires the `uds` build feature. Restrict
# callers by peer credentials (each list AND across, OR within; empty = no
# constraint).
[server.unix]
bind = "/run/moq/internal.sock"

[server.unix.allow]
uid = [1001]
# gid = [2000]
# pid = [12345]
```

No-JWT connections on the stream transports resolve through the same
public-access rules as tokenless QUIC clients (see [`[auth]`](#auth) `public`).
See [Stream Listeners](/bin/relay/auth#stream-listeners) for details.

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

Per-node stats publishing. When enabled, the relay publishes stats broadcasts
carrying JSON snapshots of the broadcasts it's currently serving and of the
sessions currently connected to it. By default, it publishes a single
`<prefix>/node/<node>` broadcast (or `<prefix>/node` when `node` is unset).
Set `depth` to bucket stats by the first N broadcast path segments and publish
one broadcast per bucket at `<prefix>/<bucket>/node/<node>`.

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

# Number of leading broadcast path segments to bucket stats by (defaults to 0).
# Set to 1 for one stats broadcast per first path segment, e.g. per tenant.
depth = 1
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
| `<tier>/publisher.json`    | named-tier egress (e.g. `region/sjc/publisher.json`) |
| `<tier>/subscriber.json`   | named-tier ingress                          |
| `sessions.json`             | default-tier connected sessions, keyed by root |
| `<tier>/sessions.json`     | named-tier connected sessions, keyed by root |

Each track also has a compressed sibling with a `.z` suffix (e.g.
`publisher.json.z`) carrying the same data for a fraction of the bytes. It's
encoded by [moq-json](https://docs.rs/moq-json): each group starts with a full
snapshot and continues with RFC 7396 merge-patch deltas, all DEFLATE-compressed
in one shared window. Read it with the
[moq-stats](https://docs.rs/moq-stats) consumer (or `moq-json` directly), not
as raw JSON frames; the plain `.json` tracks remain one full JSON object per
frame.

The default-tier tracks always exist (emitting `{}` while idle). A named tier's
tracks are created the first time traffic routes to that label.

All traffic records on the default unprefixed tier unless configured otherwise.
Use `--cluster-tier` for relay-to-relay dials, `--auth-mtls-tier` for mTLS peers
when the auth API does not return a `tier`, or the auth API's `tier` field to
select a named tier.

Each per-broadcast frame is a JSON object mapping broadcast path to a
cumulative counter snapshot. An entry surfaces on any tick where the
broadcast is live (any open counter still exceeds its `*_closed`
counterpart, so a subscription could begin at any moment) or its snapshot
changed since the previous tick. Once every counter equals its `*_closed`
counterpart no traffic can flow, so the entry is dropped:

```json
{
  "demo/bbb": {
    "announced": 1, "announced_closed": 0, "announced_bytes": 8,
    "broadcasts": 1, "broadcasts_closed": 0,
    "subscriptions": 5, "subscriptions_closed": 2,
    "fetches": 3,
    "bytes": 12345, "frames": 678, "groups": 9, "datagrams": 2
  },
  "anon/foo": {
    "announced": 1, "announced_closed": 0, "announced_bytes": 8,
    "broadcasts": 1, "broadcasts_closed": 0,
    "subscriptions": 2, "subscriptions_closed": 0,
    "fetches": 0,
    "bytes": 234, "frames": 12, "groups": 1, "datagrams": 0
  }
}
```

Field semantics:

- `announced` / `announced_closed`: cumulative count of every broadcast
  announce/unannounce event on this `(tier, role)` slot, regardless of
  whether any subscription happened. Use this for "all known broadcasts".
- `announced_bytes`: cumulative broadcast-name length summed over each
  model-visible announce and unannounce of this broadcast. It counts the name,
  not the encoded message size, so a broadcast isn't charged for hop chains or
  framing overhead (and the count is the same across protocol versions).
  Separate from `bytes`, which is media payload. Announce control traffic that
  never enters the model (auth-rejected or unmatched-prefix announcements) is
  not counted.
- `broadcasts` / `broadcasts_closed`: per-(broadcast, session)
  subscription sentinel. The first active subscription a peer session
  opens for a broadcast bumps `broadcasts`; the last one it closes bumps
  `broadcasts_closed`. Summed across sessions, `broadcasts -
  broadcasts_closed` is the number of distinct sessions currently
  subscribed to the broadcast (i.e. viewers on the egress side), which is
  typically what billing and UI want.
- `subscriptions` / `subscriptions_closed`: cumulative count of
  track-level subscriptions opened and dropped.
- `fetches`: cumulative one-shot group fetches requested by a calling session,
  counted once per coalesced fetch when the request is issued, so a fetch that
  resolves to "not found" still counts. It is separate from `subscriptions` and
  the viewer sentinel; the fetched payload still flows into `bytes` / `frames` /
  `groups`.
- `bytes` / `frames` / `groups`: cumulative payload counters, bumped as
  groups/frames are read out of the model on the egress side and written into
  it on the ingress side. Egress bytes are counted when read out of the model
  (into the QUIC send path), so bytes read but lost to a mid-group stream reset
  still count. For a fan-out egress reader (e.g. an HLS/DASH muxer) this is
  bytes read once per segment at the broadcast origin, not per downstream HTTP
  client.
- `datagrams`: cumulative single-frame groups delivered over an unreliable QUIC
  datagram (moq-lite-05+ on a datagram-capable transport). A subset of `groups`:
  each datagram also counts there, and its payload in `frames` / `bytes`. Counted
  when the datagram enters or leaves the model, so an egress datagram dropped by
  congestion or an oversized body still counts.

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

Frames for any one `(tier, role)` are skipped when nothing changed since
the last emitted frame; new subscribers still pick up a baseline
immediately via track-latest semantics.

Every flag also accepts an equivalent CLI argument (`--stats-enabled`,
`--stats-prefix`, `--stats-interval`, `--stats-node`, `--stats-depth`) and
environment variable (`MOQ_STATS_ENABLED`, `MOQ_STATS_PREFIX`,
`MOQ_STATS_INTERVAL`, `MOQ_STATS_NODE`, `MOQ_STATS_DEPTH`).

### \[cache]

Memory budget for cached groups. Old (non-latest) groups stay cached until their
track's TTL expires or the pool runs out of room, whichever comes first; under
memory pressure the least-recently-read groups are evicted first. The latest
group of every track is always retained. With neither knob set the cache is
unbounded and only the per-track TTL limits memory.

```toml
[cache]
# Maximum bytes of cached group payload. Accepts absolute sizes ("8GiB",
# "512MB") or a percentage of memory ("75%", respecting the cgroup limit
# inside containers). Unbounded when unset.
capacity = "8GiB"

# Keep at least this much system memory available ("2GiB" or "10%"). Enables a
# background governor that re-sizes the cache every few seconds: it grows into
# idle memory and shrinks (evicting) when the rest of the system needs it, so
# the cache is effectively the lowest-priority user of RAM. Combine with
# `capacity` to also cap the absolute size.
headroom = "2GiB"
```

The `capacity` budget counts group payload bytes, not process RSS, so leave
slack below physical memory (or just use `headroom`, which measures actual
available memory).

Both flags also accept CLI arguments (`--cache-capacity`, `--cache-headroom`)
and environment variables (`MOQ_CACHE_CAPACITY`, `MOQ_CACHE_HEADROOM`).

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
