---
title: moq-relay
description: Route MoQ broadcasts between publishers and subscribers
---

# moq-relay

`moq-relay` routes, caches, and fans out broadcasts without parsing their media
payloads. Run one relay for a small deployment or connect several relays into a
cluster.

## Install

Choose one installation method:

```bash
# Cargo
cargo install moq-relay

# Nix
nix run github:moq-dev/moq#moq-relay -- relay.toml

# Windows
winget install moq-dev.moq-relay
```

Container images for `linux/amd64` and `linux/arm64` are published on
[Docker Hub](https://hub.docker.com/r/moqdev/moq-relay):

```bash
docker run -p 4443:4443/udp -p 4443:4443/tcp \
  -v "$(pwd)/relay.toml:/app/relay.toml:ro" \
  moqdev/moq-relay -- /app/relay.toml
```

To build from source, clone the repository and run
`cargo build --release --bin moq-relay`.

## Configure and run

The relay takes one TOML configuration path:

```bash
moq-relay relay.toml
```

A local-only configuration can use a generated certificate and anonymous access:

```toml
[server]
listen = "127.0.0.1:4443"
tls.generate = ["localhost"]

[web.http]
listen = "127.0.0.1:4443"

[auth]
public = ""
```

The HTTP listener serves the certificate fingerprint used to verify the local
QUIC endpoint. Do not use generated certificates or unrestricted anonymous
access for a public deployment.

See the [configuration reference](/bin/relay/config) for every option and the
[`demo/relay`](https://github.com/moq-dev/moq/tree/main/demo/relay) directory for
working configurations.

## Operate a relay

| Task | Guide |
| --- | --- |
| Restrict publish and subscribe paths | [Authentication](/bin/relay/auth) |
| Connect multiple relays | [Clustering](/bin/relay/cluster) |
| Inspect broadcasts, health, and metrics | [HTTP endpoints](/bin/relay/http) |
| Configure TLS, networking, and host tuning | [Production deployment](/setup/prod) |

Set `RUST_LOG` to adjust logging without changing the configuration:

```bash
RUST_LOG=info moq-relay relay.toml
RUST_LOG=moq_relay=trace moq-relay relay.toml
```

## Troubleshooting

- **Address already in use:** check that no other process is listening on the
  configured UDP or TCP port.
- **Certificate failure:** verify the hostname, certificate chain, private key,
  and file permissions.
- **Connection timeout:** verify that UDP reaches the relay and that the client
  URL uses the configured hostname and port.
- **Authorization failure:** inspect the connection path and token permissions
  using the [authentication guide](/bin/relay/auth).
