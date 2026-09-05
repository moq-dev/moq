---
title: Configuration
description: TOML reference for moq-relay
---

# Configuration

`moq-relay relay.toml`. Every key is also a CLI flag and environment variable
(`--listen-backend`, `MOQ_LISTEN_BACKEND`), named by joining the section and key.

## \[listen]

```toml
[listen]
bind = "[::]:443"                    # QUIC (UDP), as --listen. Omit for a stream-only relay.
version = ["moq-lite-05"]            # Restrict accepted versions. Omit for all.

[listen.tls]
cert = "cert.pem"                    # Certificate chain and key. Reloaded on change.
key = "key.pem"
generate = ["localhost"]             # Or: a self-signed cert for development.
root = ["peer-ca.pem"]               # Optional: CAs whose client certs get full access (mTLS).

[listen.tcp]                         # Plaintext qmux over TCP for trusted local workers.
bind = "127.0.0.1:4444"

[listen.unix]                        # Plaintext qmux over a Unix socket, gated by peer credentials.
bind = "/run/moq/internal.sock"
allow.uid = [1001]
```

## \[quic]

Transport tuning shared by the accept and dial sides; `[listen.quic]` and
`[connect.quic]` override it for one direction.

```toml
[quic]
congestion_control = "delay"         # "delay" (BBR) or "loss" (CUBIC, the default on noq and iroh).
```

The `noq` (default), `quinn`, and `quiche` QUIC backends are compile-time
features selected with `listen.backend` / `connect.backend`. Each ships a
different BBR generation (BBRv1 on quinn, BBRv2 on quiche, BBRv3 on noq and
iroh), which is why the knob names a family rather than an algorithm.

## \[runtime]

By default one work-stealing runtime serves every connection off one UDP
socket. On Linux, QUIC can instead run on pinned single-threaded workers, each
with its own socket on the listen address (`SO_REUSEPORT`), so a connection is
handled start to finish by one thread and its packets never cross cores.

```toml
[runtime]
workers = 8                          # Single-threaded QUIC workers. Omit for the shared runtime.
pin = true                           # Pin each worker to a core. Default.
io_uring = false                     # Drive them with io_uring instead of tokio.
```

Packets are steered by connection ID, so a client that migrates stays with its
worker. `workers` needs the `noq` (default) or `quinn` backend, an explicit
non-zero `listen.bind` port, and real certificate files rather than
`tls.generate`. `io_uring` additionally needs Linux 6.12+, the `io-uring` cargo
feature, and exactly one certificate read at startup; it serves moq-lite only,
and refuses to start anywhere it cannot deliver. `[quic]` applies either way,
except that `mtu_discovery` is refused under `io_uring` (its datagram path
sends a fixed payload) rather than quietly ignored. Each worker reports its own
counters at [`/metrics`](/bin/relay/http#get-metrics).

## \[web]

```toml
[web.http]
listen = "[::]:4443"                 # HTTP: fingerprint, announced, fetch, health.

[web.https]
listen = "[::]:443"                  # HTTPS plus the WebSocket fallback.
cert = "cert.pem"
key = "key.pem"

[internal]
listen = "127.0.0.1:9101"            # Unauthenticated /health, /metrics, /nodes. Keep private.
```

See [HTTP endpoints](/bin/relay/http).

## \[auth]

```toml
[auth]
# Pick one key source:
key = "public.jwk"                   # one verification key
# key_dir = "/etc/moq/keys/"         # or a directory of {kid}.jwk files
# auth_api = "https://api.example.com/auth"   # or one call returning key, public access, alias, and tier

public = "anon"                      # Anonymous publish and subscribe under this prefix.
# [auth.public]                      # Or split them:
# subscribe = ["anon", "demo"]
# publish = ["anon"]
```

See [Authentication](/bin/relay/auth).

## \[cluster]

```toml
[cluster]
connect = ["https://us-east.example.com/?cost=10"]   # Peers to dial. ?cost prices the link.
node = "https://us-west.example.com/"                 # This relay's own URL.
mesh = true                                           # Gossip: peers discover and dial `node`.
connect_api = "https://api.example.com/peers"        # Or fetch the peer list (JSON array) live.
token = "cluster.jwt"                                 # JWT for dials without an inline ?jwt=.
id = 12345                                            # Stable Hop ID across restarts.

[cluster.lan]                                         # Find peers on the LAN over mDNS.
enabled = true
secret = "/etc/moq/cluster.key"                       # Required: 64 hex chars, or a file holding them.
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
```

A draining upstream may name a replacement URI. `same-host` follows it only
onto the host we already dialed, so a peer moves us between ports and schemes;
`follow` also lets it choose the host, which means trusting it not to point us
into the local network, since a name it controls resolves wherever it likes;
`ignore` redials the configured URL. `handover` is a cap: a shorter deadline on
the received GOAWAY wins, a longer one does not extend it.

## \[cache]

```toml
[cache]
capacity = "8GiB"                    # Target bytes of cached groups. "75%" of memory also works.
headroom = "2GiB"                    # Or: keep this much system memory free and grow into the rest.
duration = "30s"                     # Cap how long a non-latest group is kept, whatever the publisher asked.
```

`duration` defaults to 30s and bounds memory by age, where `capacity` bounds it
by bytes. The latest group of every track is always kept. The two reclaim
differently: the byte budget is repaid as tracks write, so it caps what *active*
publishers build up, while `duration` sweeps on a wall-clock cadence, so a
publisher that stalls but stays connected still has its idle groups reclaimed
(an open one included, and a subscriber parked inside it is told rather than
waiting forever).

## \[stats]

```toml
[stats]
enabled = true
prefix = ".stats"                    # Broadcasts appear under <prefix>/node/<node>.
interval = 1                         # Seconds between snapshots.
node = "sjc/1"                       # Disambiguates relays sharing a cluster.
depth = 1                            # Also bucket by the first N path segments (per tenant).
```

Each stats broadcast carries `publisher.json`, `subscriber.json`, and
`sessions.json` tracks (plus compressed `.z` twins) with cumulative counters
per broadcast: bytes, frames, groups, datagrams, subscriptions, announces, and
connected sessions. Traffic is split by an arbitrary **tier** label chosen by
the auth API, `--cluster-tier`, or `--auth-mtls-tier`, which is what makes
billing per customer or per region possible. Read them with the
[`moq-stats`](https://docs.rs/moq-stats) crate.

## \[iroh]

```toml
[iroh]
enabled = true
secret = "./iroh-secret.key"         # Persist the key so the endpoint id survives restarts.
# disable_relay = true               # Direct addresses only. Right on a LAN, wrong on the internet.
```

See [Transport](/concept/transport#iroh-peer-to-peer-experimental).

## \[log]

```toml
[log]
level = "info"                       # RUST_LOG overrides this.
```
