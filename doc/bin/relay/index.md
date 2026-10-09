---
title: moq-relay
description: The MoQ relay server
---

# moq-relay

`moq-relay` routes broadcasts from publishers to subscribers. It caches
groups, merges duplicate subscriptions, and never parses the media, so one
relay serves video, audio, and data alike.

## Features

- **QUIC, WebTransport, and WebSocket** listeners, so browsers and native clients connect to one process.
- **Path-scoped authentication** with JWTs, mTLS for peers, and anonymous patterns, decided by an auth server or a static grant. See [Authentication](/bin/relay/auth).
- **Clustering** across hosts and regions with hop-list routing, per-link costs, LAN discovery, and dynamic peer lists. See [Clustering](/bin/relay/cluster).
- **A group cache** with byte and age budgets, so late joiners and the HLS gateway can fetch recent history.
- **HTTP endpoints** to list broadcasts, fetch groups, probe health, and scrape Prometheus metrics. See [HTTP](/bin/relay/http).
- **Live stats** published as MoQ tracks per node and per tenant, split by billing tier.
- **Plaintext TCP and Unix-socket listeners** for trusted local workers, and experimental [iroh](/concept/transport#iroh-peer-to-peer-experimental) peer-to-peer.
- **Hot reload** of certificates and trust roots.

## Run

```bash
cargo install moq-relay          # or brew, apt, dnf, winget, docker; see Install
moq-relay relay.toml
```

The `.deb` and `.rpm` systemd service reads `/etc/moq-relay/relay.toml`
using the same positional config argument.

The relay takes one TOML file. A local development config:

```toml
[listen]
bind = "[::]:4443"
tls.generate = ["localhost"]

[web.http]
listen = "[::]:4443"   # serves the certificate fingerprint for local browsers

[auth]
public = "**"          # anonymous access to everything; development only
```

Every option is also a `--flag` or `MOQ_*` environment variable, and
`RUST_LOG` controls logging. The
[configuration reference](/bin/relay/config) covers every section, and
[`demo/relay/`](https://github.com/moq-dev/moq/tree/main/demo/relay) has
working configs for development, production, and a cluster.

## Embed

`moq-relay` is also a library, for an application that wants extra HTTP
routes or in-process workers against the cluster origin. Load a `Relay`, take
what you need, and call `run`, which keeps the listeners, QUIC workers, and
shutdown inside the relay.

```rust
use axum::routing::get;
use moq_relay::{Config, Relay};

let relay = Relay::load(config).await?;
let origin = relay.cluster().origin.clone();
let trigger = relay.shutdown_trigger().clone();
let web = relay.web().routes().route("/hello", get(|| async { "hello" }));
relay.with_web(web).run().await?;
```

- `Relay::load` binds every socket, so a taken port fails there, and the
  accessors report the bound addresses, including ports assigned for `:0`.
- `run` consumes the relay, so clone the handles your tasks need first.
- Build on `web().routes()`: `with_web` replaces the router, so `Router::new()`
  drops the built-in routes.
- `run` drains on SIGTERM or SIGINT. To own the signals, for example to leave
  DNS before draining, call `with_signals(false)` and fire `shutdown_trigger()`
  yourself. The drain reaches only MoQ sessions, so stop any extra listener
  (RTMP, SRT, ...) on your own deadline.
- To decide admissions yourself, leave `[auth]` empty and answer
  `relay.admissions()`; see [Authentication](/bin/relay/auth#in-process).

See [docs.rs/moq-relay](https://docs.rs/moq-relay) and
[`rs/moq-relay/examples/embed.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-relay/examples/embed.rs).

## Operate

| Task | Guide |
| --- | --- |
| Expose it publicly with TLS and host tuning | [Production deployment](/setup/prod) |
| Decide who may publish and subscribe where | [Authentication](/bin/relay/auth) |
| Add more relays | [Clustering](/bin/relay/cluster) |
| Monitor, debug, fetch history | [HTTP endpoints](/bin/relay/http) |

## Troubleshooting

- **Address already in use**: something else holds the UDP or TCP port.
- **Certificate errors**: the hostname must match the certificate. Local browsers need the fingerprint served over `[web.http]`.
- **Connection timeout**: UDP isn't reaching the relay, or the client URL names the wrong port.
- **Unauthorized / forbidden**: the token's paths don't cover the connection path, or the broadcast a session asked for. See [path matching](/bin/relay/auth#path-matching).
