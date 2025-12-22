---
title: moq-relay
description: Clusterable relay server for MoQ
---

# moq-relay

A stateless relay server that routes broadcasts between publishers and subscribers, performing caching, deduplication, and fan-out.

## Overview

`moq-relay` is designed to run in datacenters, relaying media across multiple hops to improve quality of service and enable massive scale.

**Features:**
- Fan-out to multiple subscribers
- Caching and deduplication
- Cross-region clustering
- JWT-based authentication
- HTTP debugging endpoints

## Installation

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq
cargo build --release --bin moq-relay
```

The binary will be in `target/release/moq-relay`.

### Using Cargo

```bash
cargo install moq-relay
```

### Using Nix

```bash
nix build github:moq-dev/moq#moq-relay
```

## Configuration

Create a `relay.toml` configuration file:

```toml
[server]
bind = "[::]:4443"  # Listen on all interfaces, port 4443

[tls]
cert = "/path/to/cert.pem"  # TLS certificate
key = "/path/to/key.pem"    # TLS private key

[auth]
public = "anon"     # Allow anonymous access to anon/**
key = "root.jwk"    # JWT key for authenticated paths
```

See [dev.toml](https://github.com/moq-dev/moq/blob/main/rs/moq-relay/cfg/dev.toml) for a complete example.

## Running

```bash
moq-relay --config relay.toml
```

Or with the config path as the only argument:

```bash
moq-relay relay.toml
```

## HTTP Endpoints

For debugging, the relay exposes HTTP endpoints on the same bind address (TCP instead of UDP):

### GET /certificate.sha256

Returns the fingerprint of the TLS certificate:

```bash
curl http://localhost:4443/certificate.sha256
```

### GET /announced/*prefix

Returns all announced tracks with the given prefix:

```bash
# All announced broadcasts
curl http://localhost:4443/announced/

# Broadcasts under "demo/"
curl http://localhost:4443/announced/demo
```

### GET /fetch/*path

Returns the latest group of the given track:

```bash
curl http://localhost:4443/fetch/demo/video
```

::: warning
The HTTP server listens on TCP, not HTTPS. It's intended for local debugging only.
:::

## Clustering

Multiple relay instances can cluster for geographic distribution:

```toml
[cluster]
root = "https://root-relay.example.com"  # Root node
node = "https://us-east.relay.example.com"  # This node's address
```

### How Clustering Works

`moq-relay` uses a simple clustering scheme:

1. **Root node** - A single relay (can serve public traffic) that tracks cluster membership
2. **Other nodes** - Accept internet traffic and consult the root for routing

When a relay publishes a broadcast, it advertises its `node` address to other relays via the root.

### Cluster Arguments

- `--cluster-root <HOST>` - Hostname/IP of the root node (omit to make this node the root)
- `--cluster-node <HOST>` - Hostname/IP of this instance (needs valid TLS cert)

### Benefits

- Lower latency (users connect to nearest relay)
- Higher availability (redundancy)
- Geographic distribution

### Current Limitations

- Mesh topology (all relays connect to all others)
- Not optimized for large clusters (3-5 nodes recommended)
- Single root node (future: multi-root)

## Authentication

The relay supports JWT-based authentication. See the [Authentication guide](/guide/authentication) for detailed setup.

### Quick Setup

1. Generate a key:

```bash
moq-token --key root.jwk generate
```

2. Configure relay:

```toml
[auth]
key = "root.jwk"
public = "anon"  # Optional: allow anonymous access to anon/**
```

3. Generate tokens:

```bash
moq-token --key root.jwk sign \
  --root "rooms/123" \
  --publish "alice" \
  --subscribe "" \
  --expires 1735689600 > alice.jwt
```

4. Connect with token:

```
https://relay.example.com/rooms/123?jwt=<token-content>
```

### Token Claims

- `root` - Root path for all operations
- `pub` - Publishing permissions (path suffix)
- `sub` - Subscription permissions (path suffix)
- `cluster` - Cluster node flag
- `exp` - Expiration (unix timestamp)

### Anonymous Access

To allow anonymous access to a path prefix:

```toml
[auth]
public = "anon"  # Allow access to anon/** without token
key = "root.jwk"  # Require token for other paths
```

To make everything public (not recommended):

```toml
[auth]
public = ""  # Allow access to all paths
```

## TLS Setup

The relay requires TLS certificates. Use [Let's Encrypt](https://letsencrypt.org/):

```bash
# Install certbot
sudo apt install certbot  # Ubuntu/Debian
brew install certbot      # macOS

# Generate certificate
sudo certbot certonly --standalone -d relay.example.com
```

Update `relay.toml`:

```toml
[tls]
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"
```

## Production Deployment

See the [Deployment guide](/guide/deployment) for:

- Running as a systemd service
- Cloud deployment (Linode, AWS, GCP, etc.)
- Multi-region clustering
- Monitoring and logging
- Performance tuning

## Monitoring

### Logging

Set log level via environment variable:

```bash
RUST_LOG=info moq-relay relay.toml
RUST_LOG=debug moq-relay relay.toml
RUST_LOG=moq_relay=trace moq-relay relay.toml
```

### Metrics

Metrics (Prometheus format) are planned but not yet implemented.

Current visibility:
- Check logs for connection count
- Use HTTP endpoints for track inspection
- Monitor system resources (CPU, memory, bandwidth)

## Performance

### Current Status

- **Single-threaded** - Quinn uses one UDP receive thread
- **In-memory caching** - Recent groups stored in RAM
- **Mesh clustering** - All relays connect to all others

### Scaling

- **Vertical** - Fast CPU matters more than core count
- **Horizontal** - Deploy multiple relays in different regions
- **Cluster size** - 3-5 nodes optimal with current implementation

### Future Improvements

- Multi-threaded UDP processing
- Tree-based clustering topology
- Improved memory management
- Metrics and observability

## Troubleshooting

### Port Already in Use

```bash
# Check what's using port 4443
lsof -i :4443

# Kill the process or use a different port
```

### Certificate Errors

Ensure:
- Certificate is valid and not expired
- Certificate matches domain name
- Private key has correct permissions
- Certificate includes full chain

### Connection Timeouts

Check:
- UDP port is open in firewall
- Cloud provider allows UDP traffic
- TLS certificate is valid
- Relay is actually running

## Next Steps

- Set up [Authentication](/guide/authentication)
- Deploy to production ([Deployment guide](/guide/deployment))
- Use [moq-lite](/rust/moq-lite) client library
- Build media apps with [hang](/rust/hang)
