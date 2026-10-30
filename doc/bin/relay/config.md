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
implementation = "edge/1"            # SETUP name for the relay. Omit for moq-relay/<v> moq-net-rs/<v>, "" to send none.

[listen.reset]
secret = "/etc/moq/reset.key"        # File holding 64 hex chars. Omit for a random key.

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
                                     # The relay always offers ietf, for moq-lite-07 relay links.
auth = false                         # Decline MoQ Auth.
solicit = false                      # Decline MoQ Solicit, so peers may announce unasked.
epoch = false                        # Decline MoQ Epoch, so routes over moq-transport carry no epoch.
```

`timeout` bounds the whole handshake, transport through MoQ SETUP and auth
admission, so a peer that connects and never speaks, or whose admission never
gets an answer, is closed instead of held open by keep-alives. An HTTP read's
admission is bounded by the same value from the request's arrival.

`implementation` names the relay in its SETUP, on accepted sessions and cluster dials
alike. A client that advertises its own name (moq-lite-05+ or any moq-transport draft)
reaches the [auth server](/bin/relay/auth#the-contract) with it as `implementation`,
which is where to count client versions; older clients arrive without one.

`reset.secret` lets a relay restarted on the same address reset its
predecessor's QUIC connections, so clients reconnect at once instead of
waiting out the idle timeout. Keep it secret, and give it to one relay
only: any holder resets every other holder's connections, so live relays behind
one address would reset each other's clients unless the load balancer routes by
connection ID (`[listen.lb] id`, QUIC-LB). Relays on different addresses gain nothing from sharing
it, since a client accepts a reset only from its relay's address.
Create one with `openssl rand -hex 32 > reset.key`.

A setting no configured listener reads stops startup rather than being ignored.
A stream-only relay refuses `preferred_v4`, `preferred_v6`, `lb.id`, and
`reset.secret`, which only QUIC reads, and a `[listen.tls]` `cert`, `key`, or `generate` unless
`tcp.tls` serves it.
The io\_uring runtime refuses `preferred_v4`, `preferred_v6`, and `lb.id`;
use the tokio runtime when the listener needs these settings.
`[listen.lb] nonce` needs an `id` in the same table; unset nonce uses 8.
The `--listen-quic-lb-id` and `--listen-quic-lb-nonce` flags update that table.
`unix.allow` needs `unix.bind`.

## \[quic]

Transport tuning, applied to accepted and dialed connections alike.

```toml
[quic]
congestion_control = "delay"         # "delay" (BBR, the default), "loss" (CUBIC), or "none".
max_streams = 10000                  # Concurrent streams per connection, bidi and uni; 0 is refused. Default.
idle_timeout = "10s"                 # Drop a connection after this long with nothing on it. Default.
keep_alive = "3s"                    # Ping interval; "0s" disables it. iroh refuses zero and keeps a separate 5s path heartbeat. Default.
gso = true                           # UDP segmentation offload. iroh cannot turn it off.
mtu_discovery = false                # Path MTU discovery. Default.
receive_window = 134217728           # Whole-connection window, in bytes. Default (128 MiB).
stream_receive_window = 4194304      # Per-stream window, in bytes. Default (4 MiB).
send_window = 16777216               # Unacknowledged outgoing data, in bytes. Default (16 MiB).
qlog = "/var/log/moq/qlog"           # Existing directory. Needs the `qlog` build feature.
# keylog = "/private/keys.log"       # Explicit secret log path; unset is off.
```

`congestion_control = "none"` runs no congestion control and no pacing. It is
meant for tests and controlled links, where nothing else shares the path.

Set `keylog` only while debugging, since the file decrypts every captured session.
Tokio and io\_uring open the configured path at initialization. New Unix files are
mode 0600; an existing file readable by group or others is refused without changing
its permissions. `SSLKEYLOGFILE` does not configure native relay logging.
Iroh currently refuses an explicit keylog path when enabled.

`idle_timeout` is how long a peer that vanished without a close keeps its
sessions, and so its [cluster routes](/bin/relay/cluster#failure-detection).
QUIC uses the smaller of the two endpoints' values, so this also bounds the
other end. Keep `keep_alive` under a third of it.

Raise the receive windows when a fat, long path idles below the link rate. The
128 MiB `receive_window` default carries several Gbps at a 100 ms RTT and bounds
how much unread data and incomplete messages one peer can make the relay
buffer. It has to fit the largest message, just over 64 MiB, so a smaller
value, here or in `[limits.peer.quic]`, is refused at startup. When incomplete
messages fill the window, the relay cancels the least urgent stream: the
lowest subscription priority, then the group its subscription would deliver
last. Over iroh the window bounds only
unread data, and over WebSocket or TCP nothing bounds incomplete messages.
`stream_receive_window` only paces one stream: its credit returns as bytes are
read, whether or not their message is complete.

## \[limits.client] and \[limits.peer]

Session limits apply separately to each session on every transport. The relay
uses `[limits.peer]` for an admitted grant marked `peer` or `upstream`, including
verified LAN membership, and for links it dials through the cluster. A client
certificate, route hop, or cluster-looking URL alone does not select peer limits.

```toml
[limits.client]
announces = 1000                     # Simultaneous broadcasts or namespaces.

[limits.peer]
announces = 100000

[limits.peer.quic]
max_streams = 100000                 # Concurrent QUIC streams per direction.
receive_window = 671088640           # Connection-wide receive window (640 MiB).
send_window = 671088640              # Unacknowledged outgoing data (640 MiB).
```

`[limits.peer.quic]` takes only `max_streams`, `receive_window`, and `send_window`;
any other `[quic]` key is refused. They replace the `[quic]` values for a
peer's QUIC connection once its grant is known, before the SETUP reply. Links
the relay dials get them from the handshake on. A link carrying a whole cluster is then
not throttled by `[quic]` limits sized for one viewer, for example
`--limits-peer-quic-max-streams=200000`. Peers over WebSocket, TCP, or iroh keep
their transport's own limits.

These are the defaults. Each subscription and FETCH holds a stream, as does each
PUBLISH a peer's track listing gets, so `[quic]` or `[limits.peer.quic]`
`max_streams` bounds them, and a peer past it waits rather than fails. The
removed `requests` limit is refused at startup. Size peer limits
for the combined traffic they carry, and use session peak counters to check
announcement and subscription headroom. Exceeding `announces` closes the session
with `TOO_MANY_REQUESTS`; reconnecting with the same load will exceed it again.
Zero admits no entries of that kind, not unlimited entries.

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
shares the same reloadable TLS identity and serves every configured version
like the shared runtime. A setting the build or host cannot deliver refuses to
start rather than being ignored. If io\_uring workers fail to start naming
`RLIMIT_MEMLOCK`, raise it (`LimitMEMLOCK=` under systemd).

## \[web]

```toml
[web.http]
listen = "[::]:4443"                 # HTTP: fingerprint, announced, fetch, health.

[web.https]
listen = "[::]:443"                  # HTTPS plus the WebSocket fallback.
cert = "cert.pem"                    # cert, key, and root need listen. Omit cert and key with [acme].
key = "key.pem"

[internal]
listen = "127.0.0.1:9101"            # Unauthenticated /health, /metrics, /nodes, /sessions, POST /sessions/revalidate. Keep private.
```

`listen.timeout` also closes HTTP connections with no complete request in flight,
including HTTPS peers that stall the TLS handshake
and HTTP/2 peers that acknowledge keep-alive PINGs. Active response bodies
keep the connection open; a WebSocket upgrade hands its lifetime to MoQ.

See [HTTP endpoints](/bin/relay/http).

## \[acme]

The relay provisions and renews a publicly trusted certificate itself, for QUIC
and HTTPS alike (Unix only, where the key can be kept owner-only). This needs
the default `acme` Cargo feature; builds without it refuse configured issuance:

```toml
[acme]
domains = ["relay.example.com"]      # Enables ACME. No wildcards.
contact = ["mailto:ops@example.com"] # Optional account contact.
cache = "/var/lib/moq-relay/acme"      # Required: account key and certificate, kept owner-only.
# directory = "https://acme-staging-v02.api.letsencrypt.org/directory"  # Default: Let's Encrypt production.
# timeout = "5m"                     # How long the first issuance may take before startup fails.
```

Validation is TLS-ALPN-01 only: the directory connects to TCP port 443 of each
domain, so `web.https.listen` is required and must be reachable there, directly
or forwarded. `[acme]` cannot be combined with `listen.tls.cert`, `key`, or
`generate`, or with `web.https.cert` and `key`.

With no usable certificate in `cache` (missing, expired, missing a domain, or
from another directory), startup answers only the challenge until one is
issued; QUIC, the other routes, and readiness wait for it, and startup fails
with the ACME error once `timeout` runs out. It also fails after 3 failed
validations within an hour, counting earlier starts and renewals recorded in
`cache`, so a restart loop stops short of Let's Encrypt's limit of 5 per
hostname per hour. Renewal runs in the background well before expiry and
reaches every listener on its next handshake. A failed renewal keeps the
current certificate serving and retries, pausing once the same budget is spent
until the oldest failure is an hour old; a restart in the meantime still serves
the cached certificate.

## \[stun]

Build with `cargo install moq-relay --features stun` to use the relay as an
explicitly selected STUN server for WebRTC ICE. This feature is off by default.
The listener has its own UDP port and is disabled unless configured:

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
```

See [Authentication](/bin/relay/auth).

## \[cluster]

```toml
[cluster]
connect = ["https://us-east.example.com/?cost=10"]   # Peers to dial. ?cost prices the link, or use {url, cost, egress, token, upstream} objects.
node = "https://us-west.example.com/"                 # This relay's own URL.
token = "cluster.jwt"                                 # JWT for dials without an inline ?jwt=.
id = 12345                                            # Stable Hop ID across restarts.
max_hops = 32                                         # Longest accepted/forwarded hop path (1 to 32).

[cluster.api]
url = "https://api.example.com/peers"                  # Live JSON peer list; a local file also works.
# tls.root = ["api-ca.pem"]                           # API server trust, independent from connect.tls.

[cluster.lan]                                         # Find peers on the LAN over mDNS.
enabled = true
# secret = "/etc/moq/cluster.key"                     # Optional: a file holding 64 hex chars.
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
extensions.solicit = false           # Decline MoQ Solicit.
extensions.epoch = false             # Decline MoQ Epoch. All default to on, as in [listen.extensions].
```

A draining upstream may name a replacement URI. `same-host` follows it only
onto the host already dialed, so a peer can move us between ports and schemes;
`follow` also lets it choose the host, which trusts it not to point us into
the local network; `ignore` keeps the current address. `handover` caps how long
the old connection keeps serving after a GOAWAY.

## \[cache]

```toml
[cache]
capacity = "8GiB"                    # Target bytes of cached groups and buffered datagrams. "75%" of memory also works.
headroom = "2GiB"                    # Or: keep this much system memory free and grow into the rest.
duration = "30s"                     # Cap how long a non-latest group is kept, whatever the publisher asked.
```

`capacity` is a byte target, repaid as active publishers write. `duration`
(30s by default) bounds memory by age, and sweeps on a timer, so a publisher
that stalls but stays connected still has its idle groups reclaimed. The latest
group of every track, and any group still being read, is kept past `capacity` but
sheds its oldest frames instead. Whatever the capacity, a group holds at most its
newest 32 MB. A reader of a track's newest group that reaches a shed frame asks
for a new group on tracks that opt into refresh. Any other reader of a shed
frame sees `Evicted`.

## \[stats]

```toml
[stats]
enabled = true
prefix = ".stats"                    # The broadcast appears at <prefix>/node/<node>.
interval = "1s"                      # Time between snapshots. Default.
node = "sjc/1"                       # Disambiguates relays sharing a cluster.
depth = 1                            # One group track per first N path segments (per tenant).
linger = "5m"                        # Keep an empty group's track listed this long. Default.
```

Each node publishes its traffic and session counters as MoQ tracks, split by a
**tier** label from the auth server's grant (`--cluster-tier` for links this
relay dials and LAN peers it admits), which is what makes billing per customer or per region possible.
Each run announces under a fresh [epoch](/concept/moq-lite#publisher-epochs),
so a restart is a new broadcast at the same path. Each group is a
[track](/concept/stats#group-tracks) of that broadcast, and one returning after
its linger restarts its counters under a new epoch without an announce. The
node's totals count everything since its epoch began, so billing reads those;
a group's per-path traffic and per-label sessions and HTTP requests are for
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

The secret path must be a regular file, not a symlink. A missing file is created
exclusively, with mode `0600` on Unix. Existing Unix files readable by group or
others are refused; use `chmod 600` to make an existing key private before startup.

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

The relay exits as soon as every admitted connection has left, or immediately on
a second signal. When the window ends, it stops waiting for connections still
pending and exits once every session it closed has reported its `end` to the
auth server, each report bounded by that server's request timeout. Once it commits to exit, WebSocket
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
