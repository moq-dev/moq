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
[listen]
bind = "0.0.0.0:4443"

[listen.tls]
cert = "cert.pem"
key = "key.pem"
```

## Full Reference

### Top-level keys {#drain-timeout}

```toml
# How long accepted sessions may keep running after a shutdown signal
# (SIGINT/ctrl-c, or SIGTERM on unix). The first signal sends every session a
# GOAWAY (telling clients to reconnect) and waits this long before force-closing
# them; a second signal exits immediately. Default: "10s".
drain_timeout = "10s"
```

### \[log]

Logging configuration.

```toml
[log]
# Log level: trace, debug, info, warn, error
# The RUST_LOG environment variable takes precedence
level = "info"
```

### \[runtime]

How the relay lays its QUIC work out over threads. By default one work-stealing
runtime serves every connection off one UDP socket.

```toml
[runtime]
# Serve QUIC from this many single-threaded workers instead of the shared
# runtime. Each worker is a thread with its own socket on the listen address
# (SO_REUSEPORT), and a connection is handled start to finish by one worker, so
# its packets never cross threads. Everything else (HTTP, WebSocket, tcp/unix,
# clustering) stays on the shared runtime. Omit to keep QUIC there too.
#
# Packets are steered to their worker by connection ID, not by address, so a
# client that migrates (a NAT rebinding, a network change) stays with the worker
# that owns its connection.
#
# Linux only. Needs a backend whose connection IDs can name the owning worker,
# so listen.backend must be quinn (the default) or noq; quiche refuses to start.
# Cannot be combined with listen.quic_lb_id, which wants the same bytes of the
# connection ID.
#
# The listen address needs an explicit non-zero port: an ephemeral bind gives
# each worker a port of its own instead of a shared one.
#
# Incompatible with listen.tls.generate, since each worker would generate a
# certificate of its own. Point at real certificate files instead. Each worker
# loads and watches those files itself, so a rotation is not atomic across the
# group: for as long as the reloads take, two workers can be serving different
# certificates. See https://github.com/moq-dev/moq/issues/2924.
workers = 8

# Pin each worker to a CPU core. Default: true.
pin = true
```

The shared runtime is still there for everything that is not QUIC, and it still
sizes its thread pool to the machine. Set `TOKIO_WORKER_THREADS` in the
environment to bound it, so the workers are not competing with a full second
pool for the same cores.

Load is not perfectly even across workers. A connection is assigned to a worker
by the kernel's hash of its first packet and stays there for life, so worker
load carries the binomial spread of that assignment: with ~100 connections on 4
workers, expect the busiest to carry 1.1-1.6x the idlest. Size `workers`
expecting somewhat less than one full core of capacity per worker; the spread
narrows as connection counts grow.

### \[listen]

QUIC/WebTransport server settings. Optionally add plaintext qmux stream
listeners for trusted local workers. Every connection authenticates through the
same JWT / public-access path; QUIC additionally accepts an mTLS client
certificate, and Unix sockets add optional peer-credential gating.

```toml
[listen]
# QUIC (UDP) bind. Omit to run stream-only (no QUIC) when a tcp/unix listener
# is configured below.
bind = "[::]:443"

# MoQ versions accepted by QUIC, WebTransport, and WebSocket listeners.
# TCP and Unix stream listeners also accept moq-lite-05 because it carries
# their request path in SETUP. Omit to accept every supported version.
version = ["moq-transport-16"]

# Plaintext qmux over TCP (no TLS, carries no peer identity). Trusted networks
# only; a non-loopback bind logs a warning. Requires the `tcp` build feature.
[listen.tcp]
bind = "127.0.0.1:4444"

# Plaintext qmux over a Unix socket, for local workers (e.g. the protocol
# gateways or a stats publisher). Requires the `uds` build feature. Restrict
# callers by peer credentials (each list AND across, OR within; empty = no
# constraint).
[listen.unix]
bind = "/run/moq/internal.sock"

[listen.unix.allow]
uid = [1001]
# gid = [2000]
# pid = [12345]
```

No-JWT connections on the stream transports resolve through the same
public-access rules as tokenless QUIC clients (see [`[auth]`](#auth) `public`).
See [Stream Listeners](/bin/relay/auth#stream-listeners) for details.

### \[listen.tls]

TLS configuration for the QUIC endpoint.

```toml
[listen.tls]
# Option 1: Provide certificate files
cert = "/path/to/cert.pem"   # Certificate chain
key = "/path/to/key.pem"     # Private key

# Option 2: Generate self-signed certificates (development only)
generate = ["localhost", "127.0.0.1"]

# Optional: root CAs to accept for mTLS peer authentication.
# Clients that present a cert signed by one of these CAs are granted
# full access (publish/subscribe/cluster). Intended for relay clustering.
root = ["/path/to/peer-ca.pem"]
```

For production, use certificates from Let's Encrypt or another CA. The Quinn
and Noq backends watch certificate, key, and root CA files and reload them for
new connections. Existing connections keep the identity established by their
original handshake. The Quiche backend reloads outbound client roots but
requires a relay restart after rotating its inbound TLS files.

### \[web.http]

HTTP server for debugging endpoints.

```toml
[web.http]
# Listen address for HTTP (TCP)
# Defaults to disabled if not specified
bind = "0.0.0.0:4443"
```

See [HTTP Endpoints](/bin/relay/http) for available endpoints.

### \[internal]

Plain HTTP listener for unauthenticated operational endpoints. It is disabled
unless `listen` is configured. Bind it only to loopback or a trusted private
network.

```toml
[internal]
listen = "127.0.0.1:9101"
```

It serves `/health`, Prometheus traffic and listener counters at `/metrics`, and
the local cluster topology view at `/nodes`. See
[HTTP Endpoints](/bin/relay/http) for the response formats.

### \[web.https]

HTTPS/WSS server for TCP fallback.

```toml
[web.https]
# Listen address for HTTPS/WSS (TCP)
listen = "0.0.0.0:443"

# TLS certificates (can be the same as listen.tls)
cert = "cert.pem"
key = "key.pem"

# Optional root CAs for HTTPS/WSS client certificate authentication.
root = ["/path/to/peer-ca.pem"]
```

HTTPS/WSS certificate, key, and root CA files are watched and reloaded for new
connections. A failed reload retains the last valid configuration.

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
# array of peer URLs) and reconcile it at runtime, replacing sessions when URL
# configuration such as ?cost= or ?jwt= changes.
connect_api = "https://api.example.com/cluster/connect"

# JWT for outbound cluster dials (alternative to mTLS), applied to any peer
# whose URL has no inline ?jwt=. An inline token works for static and
# connect_api-discovered peers. Gossip must use this shared token or mTLS because
# the advertised cluster.node URL is public.
token = "cluster.jwt"

[cluster.lan]
# Optional. Discover peers on the local network with mDNS instead of (or as well
# as) gossip and connect_api. Requires `node` and `secret`.
enabled = true

# The shared key admitting a peer to the LAN mesh: 64 hexadecimal characters, or
# a path to a file containing them. Every peer needs the same value. Required,
# because mDNS is unauthenticated and the relay attaches `token` to any peer it
# dials.
secret = "/etc/moq/cluster.key"
```

See [Clustering](/bin/relay/cluster) for topology choices and the trade-off between hand-listed peers and gossip.

### \[connect]

Client settings used when connecting to other relays (clustering).

```toml
[connect]
# Maximum time for one outbound dial and MoQ handshake. Defaults to 30s.
# Set to "0" to wait forever.
timeout = "30s"

# Disable TLS verification (development only!)
tls.insecure = true

# What to do with the URI an upstream peer names in its GOAWAY:
# "follow" (default), "same-host", or "ignore". A followed redirect is dialed
# exactly as given, so it must carry its own credentials; scheme downgrades and
# redirects toward loopback/private/IPC addresses are always refused.
goaway.redirect = "follow"

# How long the old upstream keeps serving after its replacement connects. This
# is a cap: a shorter deadline on the received GOAWAY wins, since the peer
# force-closes then anyway, but a longer one does not extend it. Default: "10s".
goaway.handover = "10s"

# Or provide trusted root certificates. By default these replace the system
# roots, so the relay trusts only these CAs.
# tls.root = ["/path/to/root.pem"]

# Set this to also trust the platform's system roots alongside any custom root,
# e.g. to dial a local relay with a private CA and a remote one with a public CA.
# Defaults to true only when no custom root is set.
# tls.system_roots = true

# Delay before also dialing the next resolved address (Happy Eyeballs).
# When DNS returns both IPv6 and IPv4, attempts alternate between the families,
# each starting this long after the previous one (or immediately, if that one
# fails outright), and the first connection to complete wins. "0s" dials every
# address at once. Defaults to 250ms, RFC 8305's Connection Attempt Delay.
# race = "250ms"

# Delay before dialing an IPv4 address while the full DNS answer is outstanding.
# A dial runs the usual all-families lookup alongside an IPv4-only one that
# answers without waiting for the AAAA record, and starts on the first answer, so
# a slow or dropped AAAA query no longer delays it. The full answer is
# authoritative, including which family to try first, so this is how long the
# IPv4-only one waits for it before going ahead alone. "0s" dials as soon as any
# address resolves. Defaults to 50ms, RFC 8305's Resolution Delay.
# resolution_delay = "50ms"
```

Custom client root files are watched and reloaded for new outbound connections.
If a changed file is temporarily missing, empty, or invalid, the relay retains
the last valid roots.

The connect timeout is also available as `--connect-timeout` or
`MOQ_CONNECT_TIMEOUT`, the address race as `--connect-race` or
`MOQ_CONNECT_RACE`, and the resolution delay as `--connect-resolution-delay` or
`MOQ_CONNECT_RESOLUTION_DELAY`. They compose: the resolution delay picks which
family goes first, the race staggers the attempts within one dial, and the
timeout bounds that dial as a whole.

Pinning the source port (a non-zero port in `--connect-bind`) disables address
racing on the `quiche` backend, which binds a fresh socket per attempt and so
can only dial one address at a time from a fixed port. The relay logs a warning
at startup when both are set. Leave the bind port at `0` to keep failover, or
use the `quinn` or `noq` backend, which share one socket across attempts and are
unaffected.

### \[quic]

Per-connection QUIC transport knobs. These mean the same thing whichever way a
connection was opened, so they are spelled once and shared by `[listen]` and
`[connect]` alike. The knobs that only apply when accepting (the QUIC preferred
address and QUIC-LB connection IDs) live on `[listen]` instead.

```toml
[quic]
# "loss" or "delay". Defaults per backend; don't set "delay" on noq/iroh (see below).
congestion_control = "delay"
```

`loss` is CUBIC: it grows until it drops packets, so the send rate sawtooths.
`delay` is BBR: it tracks the measured delivery rate and RTT instead of waiting
for loss, which keeps queues shorter and the send rate steady enough for a live
encoder to follow. Prefer `delay` for interactive media.

The knob names a family rather than an algorithm because each QUIC backend ships
a different BBR generation:

| Backend | `loss` | `delay` | Default when unset |
| --- | --- | --- | --- |
| quinn | CUBIC | BBRv1 | BBRv1 |
| quiche | CUBIC | BBRv2 | BBRv2 |
| noq | CUBIC | BBRv3 | CUBIC |
| iroh | CUBIC | BBRv3 | CUBIC |

noq and iroh are the exception because their shared BBRv3 can panic on packet
loss, which aborts the process. Do not select `delay` on those backends unless
you are testing that controller on purpose and can tolerate the crash.

Also available as `--quic-congestion-control` /
`--quic-congestion-control`, or `MOQ_QUIC_CONGESTION_CONTROL`.

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
    "bytes": 12345, "frames": 678, "groups": 9, "datagrams": 2,
    "stale": { "bytes": 456, "frames": 23, "groups": 4, "datagrams": 0 }
  },
  "anon/foo": {
    "announced": 1, "announced_closed": 0, "announced_bytes": 8,
    "broadcasts": 1, "broadcasts_closed": 0,
    "subscriptions": 2, "subscriptions_closed": 0,
    "fetches": 0,
    "bytes": 234, "frames": 12, "groups": 1, "datagrams": 0,
    "stale": { "bytes": 0, "frames": 0, "groups": 0, "datagrams": 0 }
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

- `stale`: cumulative `{ bytes, frames, groups, datagrams }` skipped because the
  content drifted further behind the live edge than the subscriber's latency
  budget allows, so the relay never put it on the wire. These are disjoint from
  the top-level payload counters, which remain the backwards-compatible shape for
  delivered content. A steady rate here means subscribers are consistently behind
  the live edge, which is normal for a real-time subscription during congestion
  and a problem for one that asked to tolerate more.

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
track's retention window expires, the `duration` ceiling is reached, or the pool
runs out of room, whichever comes first. Under memory pressure each track evicts
its own stalest groups as it writes, ordered by when each was last written or
served from cache, and proportional to how much it writes, so usage converges on
the budget without any global scan; groups that FETCH requests keep hitting are
retained over ones nobody reads. The latest group of every track is
always retained. With none of the knobs set the cache is unbounded and only each
track's own window limits memory.

```toml
[cache]
# Target bytes of cached group payload, which usage converges toward as tracks
# write (not a hard limit). Accepts absolute sizes ("8GiB", "512MB") or a percentage of
# memory ("75%", respecting the cgroup limit inside containers). Unbounded
# when unset.
capacity = "8GiB"

# Keep at least this much system memory available ("2GiB" or "10%"). Enables a
# background governor that re-sizes the cache every few seconds: it grows into
# idle memory and shrinks (evicting as tracks write) when the rest of the system
# needs it, so the cache is effectively the lowest-priority user of RAM. Combine
# with `capacity` to also bound the target from above.
headroom = "2GiB"

# Maximum time a non-latest cached group is retained since it was last written
# or served from cache by a FETCH ("30s", "500ms"). Caps each track's own
# retention window: a publisher advertising a longer window is clamped down to
# this, bounding how much history a track accumulates no matter what upstream
# asks for. A FETCH cache hit restarts the clock, so actively-read history
# stays cached. The latest group of every track is always retained, as it is
# the live edge. Unbounded (each track keeps its own window) when unset.
duration = "30s"
```

The `capacity` budget counts group payload bytes, not process RSS, so leave
slack below physical memory (or just use `headroom`, which measures actual
available memory). `duration` is the age counterpart: it stops a long-running
relay from accumulating hours of history per track when the byte budget alone
leaves room for it.

All eviction happens as tracks write (there is no background reaper), so both
`duration` and the byte budget cap how much history *active* publishers build
up. A publisher that stops writing but stays connected keeps what it had cached
until it resumes or the broadcast closes; under memory pressure the byte budget
is repaid by the tracks that are still writing. A publisher that disconnects has
its groups released as soon as the broadcast closes with it.

All three flags also accept CLI arguments (`--cache-capacity`,
`--cache-headroom`, `--cache-duration`) and environment variables
(`MOQ_CACHE_CAPACITY`, `MOQ_CACHE_HEADROOM`, `MOQ_CACHE_DURATION`).

### \[iroh]

Experimental P2P support via [iroh](/concept/layer/iroh). Clients dial the relay's
endpoint id (`iroh://<endpoint-id>/<path>`) instead of a hostname. An n0 relay carries the
connection from the start and hole punching moves it onto a direct path, or fails and leaves
it there.

Prefer `https://` for anything off this relay's own network. A client reaching this relay
over the internet gains nothing from iroh (this process is already the public address, and it
caches and fans out, which an n0 relay forwarding opaque packets does not), and there's no
automatic relationship between the two: `iroh://` and `https://` are separate MoQ connections
to this relay, and dialing one never falls back to the other. That choice belongs to whoever
writes the URL.

`disable_relay = true` keeps an n0 relay out of the media path entirely. Read
[Connectivity](/concept/layer/iroh#connectivity) first: it also removes hole punching and the
probes an endpoint uses to learn its own public address, so it suits a relay meeting peers on
its own network and breaks one that clients dial over `iroh://` from elsewhere. On a cloud VM
behind 1:1 NAT it is the difference between advertising your public address and advertising a
private one nobody can route to.

Either way, discovery still publishes this endpoint's addresses to n0's `iroh.link` DNS
server, so enabling iroh at all is visible off the network.

```toml
[iroh]
# Enable iroh for P2P connections
enabled = false

# Path to persist the iroh secret key, so the endpoint id survives a restart.
# Generated on first run if the file does not exist.
secret = "./relay-iroh-secret.key"

# UDP bind addresses. Default to an ephemeral port on both families.
# bind_v4 = "0.0.0.0:4444"
# bind_v6 = "[::]:4444"

# Uncomment for direct addresses only: no n0 relay, and no hole
# punching either. Right for peers on this relay's own network; see
# the note above before enabling it on a relay that clients dial
# over iroh:// from the internet.
# disable_relay = true
```

The endpoint id is logged at startup (`iroh listening endpoint_id=...`). Without `secret`
a fresh key is generated on every start, so the id changes each restart.

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
