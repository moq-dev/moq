---
title: Configuration
description: TOML reference for moq-relay
---

# Configuration

`moq-relay relay.toml`. Every key is also a CLI flag (`--server-bind`) and
environment variable (`MOQ_SERVER_BIND`), named by joining the section and key.

## \[server]

```toml
[server]
bind = "[::]:443"                    # QUIC (UDP). Omit for a stream-only relay.
version = ["moq-lite-05"]            # Restrict accepted versions. Omit for all.

[server.tls]
cert = "cert.pem"                    # Certificate chain and key. Reloaded on change.
key = "key.pem"
generate = ["localhost"]             # Or: a self-signed cert for development.
root = ["peer-ca.pem"]               # Optional: CAs whose client certs get full access (mTLS).

[server.quic]
congestion_control = "delay"         # "delay" (BBR, default on quinn/quiche) or "loss" (CUBIC).

[server.tcp]                         # Plaintext qmux over TCP for trusted local workers.
bind = "127.0.0.1:4444"

[server.unix]                        # Plaintext qmux over a Unix socket, gated by peer credentials.
bind = "/run/moq/internal.sock"
allow.uid = [1001]
```

The `quinn` (default), `quiche`, and `noq` QUIC backends are compile-time
features selected with `backend`. Don't pick `"delay"` on `noq` or iroh: their
BBRv3 can panic on loss.

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
id = 12345                                            # Stable origin id across restarts.
linger = "5s"                                         # Keep a broadcast announced this long after its publisher vanishes.
```

See [Clustering](/bin/relay/cluster).

## \[client]

Settings for outbound dials (cluster peers, auth API).

```toml
[client]
timeout = "30s"                      # Dial plus handshake. "0" waits forever.
tls.root = ["ca.pem"]                # Trust these CAs (replaces system roots unless system_roots = true).
tls.cert = "relay.pem"               # Present a client certificate (mTLS to peers and the auth API).
tls.key = "relay.key"

[client.quic]
congestion_control = "delay"
```

## \[cache]

```toml
[cache]
capacity = "8GiB"                    # Target bytes of cached groups. "75%" of memory also works.
headroom = "2GiB"                    # Or: keep this much system memory free and grow into the rest.
duration = "30s"                     # Cap how long a non-latest group is kept, whatever the publisher asked.
```

Unset, the cache is bounded only by each track's own retention window. The
latest group of every track is always kept.

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
