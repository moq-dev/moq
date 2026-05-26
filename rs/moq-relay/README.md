# moq-relay

**moq-relay** is a server that forwards subscriptions from publishers to subscribers, caching and deduplicating along the way.
It's designed to be run in a datacenter, relaying media across multiple hops to deduplicate and improve QoS.

The only argument is the path to a TOML configuration file.
See [localhost.toml](https://github.com/moq-dev/moq/blob/main/demo/relay/localhost.toml) for an example configuration.

## Install

### Debian / Ubuntu

```bash
curl -fsSL https://apt.moq.dev/moq-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/moq-archive-keyring.gpg > /dev/null
echo "deb [signed-by=/usr/share/keyrings/moq-archive-keyring.gpg] https://apt.moq.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/moq.list
sudo apt update && sudo apt install moq-relay
```

The package drops a `moq-relay.service` systemd unit and an
`/etc/moq-relay/relay.toml` config file. See
[Linux Installation](https://doc.moq.dev/setup/linux) for the full
walkthrough.

### Fedora / RHEL / Rocky / AlmaLinux

```bash
sudo dnf config-manager --add-repo https://rpm.moq.dev/moq.repo
sudo dnf install moq-relay
```

### From crates.io

```bash
cargo install moq-relay
```

### Docker

```bash
docker pull moqdev/moq-relay
```

Multi-arch images (`linux/amd64` and `linux/arm64`) are published to [Docker Hub](https://hub.docker.com/r/moqdev/moq-relay).

## HTTP

Primarily for debugging, you can also connect to the relay via HTTP.

- `GET /certificate.sha256`: Returns the fingerprint of the TLS certificate.
- `GET /announced/*prefix`: Returns all of the announced tracks with the given (optional) prefix.
- `GET /fetch/*path`: Returns the latest group of the given track.

The HTTP server listens on the same bind address, but TCP instead of UDP.
The default is `http://localhost:4443`.
HTTPS is currently not supported.

## Clustering

To scale MoQ, you will eventually need to run multiple moq-relay instances, often in different regions.
A user connects to the nearest relay and the cluster routes broadcasts between peers behind the scenes.

**moq-relay** layers clustering on top of moq-lite: every cluster peer publishes into the same logical origin, with a hop list on each broadcast for loop detection and shortest-path preference.
There are two ways to form a cluster, which can be combined:

- **Static topology** — `--cluster-connect <peer-url>` (repeatable or comma-separated). Each peer is dialed at startup and kept alive with exponential backoff. Best for 2-5 stable nodes; no discovery.
- **Gossip discovery** — `--cluster-mesh <self-url>`. This relay advertises its URL on the cluster origin so peers reached via `--cluster-connect` discover and dial it. Pair with `--cluster-connect <rendezvous-url>` to join an existing mesh.

A relay with only `--cluster-mesh` set waits passively for inbound connections (acts as a rendezvous; no QUIC client required). A relay with both flags dials the rendezvous, gossips itself, and dials every peer it learns about.

Mesh registrations live at `.internal/origins/<url>` on the cluster origin. That namespace is mTLS-only: JWT and anonymous sessions never see or publish into `.internal/*` regardless of their declared scope.

> `--cluster-root` and `--cluster-node` were removed. If you have either in an existing config, the relay errors at startup with a message pointing at `--cluster-connect` and `--cluster-mesh`.

See [doc/bin/relay/cluster.md](https://github.com/moq-dev/moq/blob/main/doc/bin/relay/cluster.md) for the full walkthrough, including mTLS setup and a 3-node example.

## Authentication

The relay supports JWT-based authentication and authorization with path-based access control.

For detailed authentication setup, including token generation and configuration examples, see:
**[Authentication Documentation](https://github.com/moq-dev/moq/blob/main/doc/app/relay/auth.md)**

Key features:

- JWT tokens passed via query parameters (`?jwt=<token>`)
- Path-based authorization with `root`, `pub`, and `sub` claims
- Anonymous access support for public content
- Symmetric key cryptography (HMAC-SHA256/384/512)
- Asymmetric key cryptography (RSASSA-PKCS1-SHA256/384/512, RSASSA-PSS-SHA256/384/512, ECDSA-SHA256/384, EdDSA)

Quick example configuration in your `.toml` file:

```toml
[auth]
key = "demo/relay/root.jwk"    # JWT signing key (relative to working directory)
public = "anon"         # Allow anonymous access to /anon prefix
```
