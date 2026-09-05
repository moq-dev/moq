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
- **Path-scoped authentication** with JWTs, mTLS for peers, anonymous prefixes, and an optional auth API for dynamic policy. See [Authentication](/bin/relay/auth).
- **Clustering** across hosts and regions with hop-list routing, per-link costs, gossip discovery, and dynamic peer lists. See [Clustering](/bin/relay/cluster).
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

The relay takes one TOML file. A local development config:

```toml
[server]
bind = "[::]:4443"
tls.generate = ["localhost"]

[web.http]
listen = "[::]:4443"   # serves the certificate fingerprint for local browsers

[auth]
public = ""            # anonymous access to everything; development only
```

Every option is also a `--flag` or `MOQ_*` environment variable, and
`RUST_LOG` controls logging. The
[configuration reference](/bin/relay/config) covers every section, and
[`demo/relay/`](https://github.com/moq-dev/moq/tree/main/demo/relay) has
working configs for development, production, and a cluster.

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
- **Unauthorized / forbidden**: the token's paths don't cover the connection path. See [path matching](/bin/relay/auth#path-matching).
