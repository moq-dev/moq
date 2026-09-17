# moq-relay

**moq-relay** is a server that forwards subscriptions from publishers to subscribers, caching and deduplicating along the way.
It's designed to be run in a datacenter, relaying media across multiple hops to deduplicate and improve QoS.

The only argument is the path to a TOML configuration file.
See [localhost.toml](https://github.com/moq-dev/moq/blob/main/demo/relay/localhost.toml) for an example configuration.

## Install

### Debian / Ubuntu

```bash
curl -fsSL https://apt.moq.dev/moq-keyring.gpg \
  | sudo tee /usr/share/keyrings/moq-keyring.gpg > /dev/null
echo "deb [signed-by=/usr/share/keyrings/moq-keyring.gpg] https://apt.moq.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/moq.list
sudo apt update && sudo apt install moq-relay
```

The package drops a `moq-relay.service` systemd unit and an
`/etc/moq-relay/relay.toml` config file. See
[Linux Installation](https://doc.moq.dev/setup/install) for the full
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

- `GET /certificate.sha256`: Returns the fingerprint of the first configured TLS certificate.
- `GET /announced/*prefix`: Returns all of the announced tracks with the given (optional) prefix.
- `GET /fetch/*path`: Returns the latest group of the given track.

The HTTP server listens on the same bind address, but TCP instead of UDP.
The default is `http://localhost:4443`.
HTTPS is currently not supported.

## Clustering

Relays can be joined together to proxy announcements and subscriptions. A viewer talks to whichever relay is closest; if their broadcast lives elsewhere in the cluster, the local relay fetches it from a neighbor and caches it. Hop tracking on every broadcast keeps loops out and picks the shortest path when there's more than one.

- `--cluster-connect <peer-url>` lists the peers this relay dials. Repeatable; defines the topology by hand. A simple chain like `eu-west <- us-east <- us-west` lets `us-east` cache and dedup the transatlantic fetches that fan out to many `us-west` viewers.
- `--cluster-mesh <self-url>` is optional. When set, this relay advertises its own URL to connected peers and dials any peers it learns about, so larger clusters don't need each node hand-configured. You still need at least one connection (in or out) so the advertisement has a path to flow. A relay with `--cluster-mesh` set and no `--cluster-connect` is a passive rendezvous.

`--cluster-root` and `--cluster-node` from earlier versions were removed. The relay errors at startup if either is set and points at the replacements.

See [doc/bin/relay/cluster.md](https://github.com/moq-dev/moq/blob/main/doc/bin/relay/cluster.md) for the full walkthrough, including topology trade-offs and authentication.

## Authentication

The relay admits a session through an auth server (`--auth-url`, one JSON
request per session event; `moq auth serve` is the reference server) or a
static anonymous grant (`--auth-public`); an application embedding the relay
can decide in process instead. A verified client certificate is reported to
the server as a fact, never a grant on its own.

For the contract, the server's flags, and token generation, see:
**[Authentication Documentation](https://github.com/moq-dev/moq/blob/main/doc/bin/relay/auth.md)**

Quick example configuration in your `.toml` file:

```toml
[auth]
url = "http://127.0.0.1:4440/"    # moq auth serve --key root.jwk --public-subscribe 'anon/**'
# public = "anon/**"              # or a static anonymous grant with no server
```
