---
title: Configuration
description: TOML reference for moq-relay
---

# Configuration

`moq-relay relay.toml`. Every key is also a CLI flag and environment variable.
Most names join the section and key: `listen.tls.cert` is
`--listen-tls-cert` / `MOQ_LISTEN_TLS_CERT`. The `listen.bind` key deliberately
uses the shorter `--listen` / `MOQ_LISTEN` spelling.
Precedence is CLI > env > file > defaults: a flag or environment variable that
was actually supplied overrides the file, and a file key that was actually
written (an empty list, a `false` boolean) overrides the built-in default.

## \[listen]

```toml
[listen]
bind = "[::]:443"                    # QUIC (UDP), as --listen. Omit for a stream-only relay.
version = ["moq-lite-05"]            # Restrict accepted versions. Omit for all.

[listen.tls]
cert = "cert.pem"                    # Certificate chain and key. Reloaded on change.
key = "key.pem"
generate = ["localhost"]             # Or: a self-signed cert for development.
root = ["peer-ca.pem"]               # Optional: CAs for client certs (mTLS), reported to the auth server.

[listen.tcp]                         # Plaintext qmux over TCP for trusted local workers.
bind = "127.0.0.1:4444"

[listen.unix]                        # Plaintext qmux over a Unix socket, gated by peer credentials.
bind = "/run/moq/internal.sock"
allow.uid = [1001]
```

## \[quic]

Transport tuning, applied to accepted and dialed connections alike.

```toml
[quic]
congestion_control = "delay"         # "delay" (BBR, the default) or "loss" (CUBIC).
max_streams = 10000                  # Concurrent streams per connection, bidi and uni. Default.
idle_timeout = "30s"                 # Drop a connection after this long with nothing on it.
keep_alive = "5s"                    # Ping interval; "0s" disables it. Ignored by iroh.
gso = true                           # UDP segmentation offload. iroh cannot turn it off.
mtu_discovery = false                # Path MTU discovery. Default.
receive_window = 67108864            # Flow-control windows, in bytes. Omit for the backend default.
stream_receive_window = 8388608
send_window = 33554432
qlog = "/var/log/moq/qlog"           # Existing directory. Needs the `qlog` build feature.
```

The native QUIC stack uses BBRv3 for delay-based congestion control. Iroh also
uses noq and the same congestion controller.

Raise the receive windows when a fat, long path idles below the link rate: a
window under the bandwidth-delay product stalls the sender waiting for credit.
Keep `stream_receive_window` well under `receive_window` so one slow group
cannot starve the connection. `send_window` caps unacknowledged outgoing data
whatever the peer allows, bounding the transport send buffer. A zero window is refused, and the receive windows must fit a QUIC
varint since they ride on the wire as transport parameters.

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
worker. The group shares one port, including an ephemeral (zero) port: the
first worker binds it and the rest join that port. Use an explicit port unless
something reads the bound address at startup. `workers` needs the `noq`
feature and real certificate files rather than `tls.generate`. A build without
QUIC rejects `workers` instead of
ignoring it. An embedding process leaves this group inside `Relay::run`;
taking the sockets out and driving them yourself is how a later library
update can drop QUIC while still compiling. `io_uring` additionally needs Linux 6.12+, the `io-uring` cargo
feature, and exactly one certificate read at startup; it serves moq-lite only,
and refuses to start anywhere it cannot deliver. `[quic]` applies either way,
except that `mtu_discovery` (its datagram path sends a fixed payload) and the
three flow-control windows (these workers run fixed ones) are refused under
`io_uring` rather than quietly ignored. Each worker reports its own counters at
[`/metrics`](/bin/relay/http#get-metrics).

## \[web]

```toml
[web.http]
listen = "[::]:4443"                 # HTTP: fingerprint, announced, fetch, health.

[web.https]
listen = "[::]:443"                  # HTTPS plus the WebSocket fallback.
cert = "cert.pem"
key = "key.pem"

[internal]
listen = "127.0.0.1:9101"            # Unauthenticated /health, /metrics, /nodes, /sessions, POST /sessions/revalidate. Keep private.
```

See [HTTP endpoints](/bin/relay/http).

## \[auth]

```toml
[auth]
# Exactly one of these:
url = "http://127.0.0.1:4440/"       # An auth server asked once per session event (`moq auth serve`,
                                     # or your own). https:// presents connect.tls; unix:// is a socket.
# public = "anon/**"                 # Or a static anonymous grant, publish and subscribe alike.
# public_subscribe = ["anon/**", "demo/**"]   # Or split them; patterns, `foo/**` for a subtree.
# public_publish = ["anon/**"]
```

See [Authentication](/bin/relay/auth).

## \[cluster]

```toml
[cluster]
connect = ["https://us-east.example.com/?cost=10"]   # Peers to dial. ?cost prices the link, or use {url, cost, egress, token} objects.
node = "https://us-west.example.com/"                 # This relay's own URL.
mesh = true                                           # Gossip: peers discover and dial `node`.
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
```

A draining upstream may name a replacement URI. `same-host` follows it only
onto the host we already dialed, so a peer moves us between ports and schemes;
`follow` also lets it choose the host, which means trusting it not to point us
into the local network, since a name it controls resolves wherever it likes;
`ignore` keeps the current address list. Empty, malformed, or refused redirects
also preserve caller-configured fallbacks; only an accepted redirect replaces
the list with the peer's URI. `handover` is a cap: a shorter deadline on
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

`headroom` starts a background task that re-samples system memory every few
seconds and resizes the pool. Embedders calling `cache::Config::init` directly
should know that the task is owned by the `cache::Pool` it resizes, not by the
`cache::Cache` struct or the `Relay`: it stops on its next tick once the last `Pool`
clone drops. Handing the `cache::Cache` to `cluster::Cluster::new` therefore moves the
task's lifetime onto the cluster, and keeping a `Pool` clone of your own keeps
the task running for as long as you hold it.

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
per broadcast. Every counter pair is `*_started` / `*_ended`:
`announces_started` / `announces_ended`, `broadcasts_started` /
`broadcasts_ended`, `subscriptions_started` / `subscriptions_ended`, and
`sessions_started` / `sessions_ended`. A live count is started minus ended.
This release also writes the previous `announced` / `*_closed` spellings beside
the new names so an older consumer still reads a new relay; a new consumer
accepts either spelling, with the canonical name winning. Payload counters
(bytes, frames, groups, datagrams) are unchanged. Traffic is split by an
arbitrary **tier** label chosen by the auth server's grant or `--cluster-tier`,
which is what makes billing per customer or per region possible. Read them with
the [`moq-stats`](https://docs.rs/moq-stats) crate.

A subscriber that wants one broadcast rather than all of them requests
`<path>/publisher.json` or `<path>/subscriber.json` (plus `.z`), or
`<tier>/<path>/...` on a named tier: the same track filtered to that one entry,
served while subscribed. The tier is matched against the labels the relay has
seen, longest first, so a default-tier broadcast whose path starts with a tier
label (`rtmp/cam` beside an `rtmp` tier) cannot be named and is refused.

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

At `info` the relay logs one `listening` record for the `[listen]` QUIC socket
and each public `[web]` listener as it binds. Each record carries the bound
address and a `kind` naming the listener:

```
INFO listening addr=[::]:4443 kind=quic
INFO listening addr=[::]:4443 kind=http
INFO listening addr=[::]:8443 kind=https
```

For these listeners, `addr` is the address the socket bound, not the one
configured, so a `listen` port of `0` reports the port the OS picked. That is
the only way to learn it from outside the process, and the QUIC and TCP ports
are chosen independently.
A relay with no `[listen]` UDP socket logs `listening (stream transports only)`
instead of the `quic` line.
