---
title: Configuration
description: TOML reference for moq-relay
---

# Configuration

`moq-relay relay.toml`. Every key is also a CLI flag and environment variable
that joins the section and key: `listen.tls.cert` is `--listen-tls-cert` /
`MOQ_LISTEN_TLS_CERT`. The exceptions: `listen.bind` is spelled `--listen` /
`MOQ_LISTEN`, and `listen.extensions` and `connect.extensions` are TOML only.
Precedence is CLI, then env, then file, then defaults.
`moq-relay --help` lists every flag.

## \[listen]

```toml
[listen]
bind = "[::]:443"                    # QUIC (UDP), as --listen. Omit for a stream-only relay.
version = ["moq-lite-05"]            # Restrict accepted versions. Omit for all.
timeout = "10s"                      # Handshake deadline. "0" waits forever.

[listen.tls]
cert = "cert.pem"                    # Certificate chain and key. Reloaded on change.
key = "key.pem"
generate = ["localhost"]             # Or: a self-signed cert for development.
root = ["peer-ca.pem"]               # Optional: CAs for client certs (mTLS), reported to the auth server. Needs QUIC.

[listen.tcp]                         # Plaintext qmux over TCP for trusted local workers.
bind = "127.0.0.1:4444"
# tls = true                         # Or: qmux over TLS (tls://) with the listen certificate, no client certs.

[listen.unix]                        # Plaintext qmux over a Unix socket, gated by peer credentials.
bind = "/run/moq/internal.sock"
allow.uid = [1001]

[listen.extensions]                  # moq-transport SETUP extensions to offer, each on by default.
auth = false                         # Decline MoQ Auth (draft 17+).
solicit = false                      # Decline MoQ Solicit, so peers may announce unasked.
```

`timeout` bounds the whole handshake, transport through MoQ SETUP, so a peer
that connects and never speaks is closed instead of held open by keep-alives.

A setting no configured listener reads stops startup rather than being ignored.
A stream-only relay refuses `preferred_v4`, `preferred_v6`, and `lb_id` (or `load_balancer`), which
only QUIC reads, and a `[listen.tls]` `cert`, `key`, or `generate` unless
`tcp.tls` serves it.
`lb_nonce` needs `lb_id`. `lb_id` and `lb_nonce` cannot be combined with `load_balancer`.
`unix.allow` needs `unix.bind`.

## \[quic]

Transport tuning, applied to accepted and dialed connections alike.

```toml
[quic]
congestion_control = "delay"         # "delay" (BBR, the default) or "loss" (CUBIC).
max_streams = 10000                  # Concurrent streams per connection, bidi and uni. Default.
idle_timeout = "10s"                 # Drop a connection after this long with nothing on it. Default.
keep_alive = "3s"                    # Ping interval; "0s" disables it. iroh still pings at least every 5s. Default.
gso = true                           # UDP segmentation offload. iroh cannot turn it off.
mtu_discovery = false                # Path MTU discovery. Default.
receive_window = 67108864            # Whole-connection window, in bytes. Default (64 MiB).
stream_receive_window = 8388608      # Per-stream window. Omit for the backend default.
send_window = 33554432               # Unacknowledged outgoing data. Omit for the backend default.
qlog = "/var/log/moq/qlog"           # Existing directory. Needs the `qlog` build feature.
```

`idle_timeout` is how long a peer that vanished without a close keeps its
sessions, and so its [cluster routes](/bin/relay/cluster#failure-detection).
QUIC uses the smaller of the two endpoints' values, so this also bounds the
other end. Keep `keep_alive` under a third of it.

Raise the receive windows when a fat, long path idles below the link rate. The
64 MiB `receive_window` default carries several Gbps at a 100 ms RTT and bounds
how much unread data one peer can make the relay buffer. Keep
`stream_receive_window` well under it so one slow group cannot starve the
connection.

## \[session] and \[peer]

Request caps apply separately to each session on every transport. The relay
uses `[peer]` for an admitted grant marked `peer` or `upstream`, including
verified LAN membership, and for links it dials through the cluster. A client
certificate, route hop, or cluster-looking URL alone does not select peer caps.

```toml
[session]
announces = 1000                     # Simultaneous broadcasts or namespaces.
subscriptions = 1000                # Simultaneously held subscriptions.
fetches = 10000                      # Independent simultaneous FETCH requests.

[peer]
announces = 100000
subscriptions = 10000
fetches = 10000
```

These are the defaults. For example, `--peer-subscriptions=50000` raises the
subscription cap on cluster links. `--session-fetches` and `--peer-fetches`
set the independent FETCH caps; concurrent callers of the same group each
hold a slot. Size peer caps for the combined traffic they carry, and use
session peak counters to check announcement and subscription headroom.
Exceeding a cap closes the session with `TOO_MANY_REQUESTS`; reconnecting with
the same load will exceed it again. Zero admits no entries of that kind, not unlimited
entries. On moq-transport drafts 14 to 16 these caps also size the request
window, so very small values can prevent other requests from progressing.

## \[runtime]

By default one work-stealing runtime serves every connection off one UDP
socket. On Linux, QUIC can instead run on single-threaded workers, each with
its own socket on the listen address (`SO_REUSEPORT`), so a connection is
handled start to finish by one worker thread and its packets never cross
threads. `pin = true` also keeps that thread on one core.

```toml
[runtime]
workers = 8                          # Single-threaded QUIC workers. Omit for the shared runtime.
pin = false                          # Pin each worker to a core. Off by default.
io_uring = false                     # Drive them with io_uring instead of tokio.
```

Packets are steered by connection ID, so a client that migrates stays with its
worker. `workers` needs the `quic` feature. Every worker and the TLS stream
listener serve one shared identity, generated or file-backed, so a certificate
rotation reaches them together and `/certificate.sha256` stays accurate.

Pinning is off by default because every relay pins its workers to the same
first cores, so two relays on one host, or one limited to fewer CPUs than the
host has, would crowd onto them. Turn it on for a host the relay has to itself.

`io_uring` additionally needs Linux 6.12+ and the `io-uring` cargo feature; it
shares the same reloadable TLS identity, serves moq-lite only, and refuses
`mtu_discovery`. A setting the build or host cannot deliver refuses to
start rather than being ignored. If io\_uring workers fail to start naming
`RLIMIT_MEMLOCK`, raise it (`LimitMEMLOCK=` under systemd).

## \[web]

```toml
[web.http]
listen = "[::]:4443"                 # HTTP: fingerprint, announced, fetch, health.

[web.https]
listen = "[::]:443"                  # HTTPS plus the WebSocket fallback.
cert = "cert.pem"                    # cert, key, and root need listen.
key = "key.pem"

[internal]
listen = "127.0.0.1:9101"            # Unauthenticated /health, /metrics, /nodes, /sessions, POST /sessions/revalidate. Keep private.
```

`listen.timeout` also closes HTTP connections with no complete request in flight,
including HTTPS peers that stall the TLS handshake
and HTTP/2 peers that acknowledge keep-alive PINGs. Active response bodies
keep the connection open; a WebSocket upgrade hands its lifetime to MoQ.

See [HTTP endpoints](/bin/relay/http).

## \[stun]

Applications can use the relay as an explicitly selected STUN server for WebRTC
ICE. The listener has its own UDP port and is disabled unless configured:

```toml
[stun]
bind = "[::]:3478"                  # Or --stun-bind / MOQ_STUN_BIND.
```

List `stun:relay.example.com:3478` in the application's ICE servers. Clients
never infer a STUN server from their relay session. This provides Binding
responses with the sender's mapped address, without TURN relaying or ICE
signaling. It is plain STUN over UDP, so it needs no TLS certificate;
`stuns:` (STUN over TLS) is not served.

The private limits are 10 requests/second per source IP with a burst of 20,
and 1000/second globally with a burst of 2000. The source table retains at most
4096 IPs; an idle entry can be replaced once its two-second bucket has refilled.
Excess, malformed, unsupported, and non-Binding datagrams receive no reply.
The internal `/metrics` endpoint reports `moq_relay_stun_replies_total` and
`moq_relay_stun_dropped_total` with `reason` values `source`, `global`,
`capacity`, `invalid`, and `io`, only when this listener is configured.
`io` counts replies that could not be sent; `moq_relay_stun_io_errors_total`
also counts asynchronous peer errors reported while receiving. Peer network
errors and interrupted calls leave the listener running; other local socket
failures stop the relay.

## \[auth]

```toml
[auth]
# Exactly one of these:
url = "http://127.0.0.1:4440/"       # An auth server asked once per session event (`moq auth serve`,
                                     # or your own). https:// presents connect.tls; unix:// is a socket.
# public = "anon/**"                 # Or a static anonymous grant rooted at /, publish and subscribe alike.
# public_subscribe = ["anon/**", "demo/**"]   # Or split them; patterns, `foo/**` for a subtree.
# public_publish = ["anon/**"]
verifications = 128                  # In-band tokens one session verifies at once; later ones wait.
```

See [Authentication](/bin/relay/auth).

## \[cluster]

```toml
[cluster]
connect = ["https://us-east.example.com/?cost=10"]   # Peers to dial. ?cost prices the link, or use {url, cost, egress, token, upstream} objects.
node = "https://us-west.example.com/"                 # This relay's own URL.
# connect_api_tls_root = ["api-ca.pem"]              # Private API roots, independent from connect.tls.
connect_api = "https://api.example.com/peers"        # Or fetch the peer list (JSON array of URLs and/or objects) live.
token = "cluster.jwt"                                 # JWT for dials without an inline ?jwt=.
id = 12345                                            # Stable Hop ID across restarts.

[cluster.lan]                                         # Find peers on the LAN over mDNS.
enabled = true
# secret = "/etc/moq/cluster.key"                     # Optional: 64 hex chars, or a file holding them.
# app = "default"                                     # DNS-SD subtype; moq-cli shares this name.
```

See [Clustering](/bin/relay/cluster).

## \[connect]

Settings for outbound dials (cluster peers, auth API).

```toml
[connect]
timeout = "30s"                      # Dial plus handshake. "0" waits forever.
tls.root = ["ca.pem"]                # Trust these CAs (replaces system roots unless system_roots = true).
tls.cert = "relay.pem"               # Present a client certificate (mTLS to peers and the auth API).
tls.key = "relay.key"
goaway.redirect = "same-host"        # How far to trust a draining peer's redirect URI.
goaway.handover = "10s"              # Cap on how long the drained upstream keeps serving.
extensions.auth = false              # Decline MoQ Auth on outbound moq-transport sessions.
extensions.solicit = false           # Decline MoQ Solicit. Both default to on, as in [listen.extensions].
```

A draining upstream may name a replacement URI. `same-host` follows it only
onto the host already dialed, so a peer can move us between ports and schemes;
`follow` also lets it choose the host, which trusts it not to point us into
the local network; `ignore` keeps the current address. `handover` caps how long
the old connection keeps serving after a GOAWAY.

## \[cache]

```toml
[cache]
capacity = "8GiB"                    # Target bytes of cached groups. "75%" of memory also works.
headroom = "2GiB"                    # Or: keep this much system memory free and grow into the rest.
duration = "30s"                     # Cap how long a non-latest group is kept, whatever the publisher asked.
```

`capacity` is a byte target, repaid as active publishers write. `duration`
(30s by default) bounds memory by age, and sweeps on a timer, so a publisher
that stalls but stays connected still has its idle groups reclaimed. The latest
group of every track is always kept, even past `capacity`.

## \[stats]

```toml
[stats]
enabled = true
prefix = ".stats"                    # Broadcasts appear under <prefix>/node/<node>.
interval = 1                         # Seconds between snapshots.
node = "sjc/1"                       # Disambiguates relays sharing a cluster.
depth = 1                            # Also bucket by the first N path segments (per tenant).
linger = "5m"                        # Keep an empty group's broadcast announced this long. Default.
```

Each node publishes its traffic and session counters as MoQ tracks, split by a
**tier** label from the auth server's grant (`--cluster-tier` for links this
relay dials and LAN peers it admits), which is what makes billing per customer or per region possible.
Each run, and each group returning after its linger, announces under a fresh
[epoch](/concept/moq-lite#publisher-epochs), so a restart is a new broadcast at
the same path. Each group's totals count everything since its epoch began, so
billing reads those; per-path traffic and per-root sessions and HTTP requests are for
dashboards. Session counters report how close sessions come to the
per-session limits, past which one is closed with `TOO_MANY_REQUESTS`.
[Stats](/concept/stats) describes the paths, tracks, and encodings; read them
with the [`moq-stats`](https://docs.rs/moq-stats) crate.

## \[iroh]

Iroh requires building the relay from source with `cargo build --release -p moq-relay --features iroh`. Published binaries, Docker images, and the Nix package leave it out.

```toml
[iroh]
enabled = true
secret = "./iroh-secret.key"         # Persist the key so the endpoint id survives restarts.
# disable_relay = true               # Direct addresses only. Right on a LAN, wrong on the internet.
```

See [Transport](/concept/transport#iroh-peer-to-peer-experimental).

## Shutdown

```toml
drain_timeout = "10s"                # Top-level key, as --drain-timeout / MOQ_DRAIN_TIMEOUT.
```

The first SIGTERM or SIGINT starts a drain: every session is sent a GOAWAY
asking it to reconnect elsewhere, including any that connect during the drain,
and is force-closed if it is still connected when the window ends. Accepted
connections still handshaking or awaiting authorization hold the drain until
they finish, fail, or reach that same deadline, where any still pending is
refused or closed. Public HTTP/1.1 connections hold the drain from TCP accept
through TLS and their first request's headers, including HLS, API, and health
requests. A non-WebSocket first request releases that hold; HTTPS h2 releases at
ALPN because extended CONNECT is disabled. A later WebSocket upgrade on the same
keep-alive connection joins the drain when its headers arrive. The internal
listener is unaffected.

The relay exits as soon as every admitted connection has left, when the window
ends, or immediately on a second signal. Once it commits to exit, WebSocket
upgrades receive 503; plain HTTP is served until the public listener stops.
`0` skips the GOAWAY.
Only moq-lite-04+ and moq-transport clients act on a GOAWAY; older ones are
closed when the window ends. An embedder can take over the signals; see
[Embed](/bin/relay/#embed).

## \[log]

```toml
[log]
level = "info"                       # RUST_LOG overrides this.
```

At `info` the relay logs a `listening` record with the bound address for each
QUIC and public `[web]` listener, so a port of `0` reports the port the OS
picked.

```
INFO listening addr=[::]:4443 kind=quic
INFO listening addr=[::]:4443 kind=http
```
