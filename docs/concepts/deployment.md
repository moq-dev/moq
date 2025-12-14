---
title: Deployment
description: Deploying MoQ relay to production
---

# Deployment Guide

This guide covers deploying `moq-relay` to production environments.

## Relay Server

The relay server routes broadcasts between publishers and subscribers. It's designed to be simple, stateless, and horizontally scalable.

### Requirements

**Minimum:**
- 2 CPU cores
- 2 GB RAM
- Public IP address
- UDP port 4443 open (or custom port)

**Recommended for production:**
- 4+ CPU cores
- 8+ GB RAM
- Geographic distribution (multiple regions)
- Load balancing / GeoDNS

### Installation

#### Using Cargo

```bash
cargo install moq-relay
```

#### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq
cargo build --release --bin moq-relay
```

The binary will be in `target/release/moq-relay`.

#### Using Nix

```bash
nix build github:moq-dev/moq#moq-relay
```

### Configuration

Create a configuration file `relay.toml`:

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

### TLS Certificates

The relay requires TLS certificates. Use [Let's Encrypt](https://letsencrypt.org/):

```bash
# Install certbot
sudo apt install certbot  # Ubuntu/Debian
brew install certbot      # macOS

# Generate certificate
sudo certbot certonly --standalone -d relay.example.com
```

Certificates will be in `/etc/letsencrypt/live/relay.example.com/`.

Update `relay.toml`:

```toml
[tls]
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"
```

::: tip
Set up automatic renewal:
```bash
sudo certbot renew --dry-run
```
:::

### Running the Relay

```bash
moq-relay --config relay.toml
```

#### As a systemd Service

Create `/etc/systemd/system/moq-relay.service`:

```ini
[Unit]
Description=MoQ Relay Server
After=network.target

[Service]
Type=simple
User=moq
WorkingDirectory=/opt/moq
ExecStart=/usr/local/bin/moq-relay --config /opt/moq/relay.toml
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl enable moq-relay
sudo systemctl start moq-relay
sudo systemctl status moq-relay
```

View logs:

```bash
sudo journalctl -u moq-relay -f
```

## Cloud Deployment

### Linode Example

The repository includes OpenTofu/Terraform configuration for Linode in the `cdn/` directory.

**Features:**
- Multi-region deployment
- Automatic clustering
- GeoDNS via Google Cloud DNS
- JWT authentication

**Setup:**

1. Copy example configuration:
```bash
cd cdn
cp terraform.tfvars.example terraform.tfvars
```

2. Fill in your credentials in `terraform.tfvars`

3. Initialize:
```bash
tofu init
```

4. Deploy:
```bash
tofu apply
```

5. Generate secrets:
```bash
mkdir -p secrets

# Generate root key
cargo run --bin moq-token -- --key secrets/root.jwk generate

# Generate cluster token (for relay-to-relay auth)
cargo run --bin moq-token -- --key secrets/root.jwk sign \
  --publish "" --subscribe "" --cluster > secrets/cluster.jwt

# Generate demo publisher token
cargo run --bin moq-token -- --key secrets/root.jwk sign \
  --root "demo" --publish "" > secrets/demo-pub.jwt
```

6. Deploy to all nodes:
```bash
just deploy-all
```

**Monitoring:**

```bash
# SSH into a node
just ssh <node>

# View logs
just logs <node>
```

**Costs:**

The example configuration uses:
- 3x `g6-standard-2` relay nodes ($25/month each)
- 1x `g6-nanode-1` publisher node ($5/month)

Total: $80/month

Adjust node count in `input.tf`.

### Other Cloud Providers

MoQ relay works on any provider with:
- UDP support
- Public IP addresses
- Linux OS

Tested providers:
- AWS EC2
- Google Cloud Compute Engine
- Azure VMs
- DigitalOcean Droplets
- Linode
- Vultr
- Hetzner

::: warning
Some providers block or rate-limit UDP. Test thoroughly before production deployment.
:::

## Clustering

Multiple relay instances can cluster for geographic distribution:

```toml
# relay.toml
[cluster]
nodes = [
  "https://us-east.relay.example.com",
  "https://eu-west.relay.example.com",
  "https://ap-south.relay.example.com"
]
```

Each relay connects to others and forwards broadcasts between regions.

**Benefits:**
- Lower latency (users connect to nearest relay)
- Higher availability (redundancy)
- Geographic distribution

**Current limitations:**
- Mesh topology (all relays connect to all others)
- Not optimized for large clusters (3-5 nodes recommended)

See [moq-relay README](https://github.com/moq-dev/moq/tree/main/rs/moq-relay) for details.

## Authentication

See the [Authentication guide](/guide/authentication) for JWT setup.

Quick example:

```bash
# Generate key
moq-token --key root.jwk generate

# Sign a token
moq-token --key root.jwk sign \
  --root "rooms/meeting-123" \
  --publish "alice" \
  --subscribe "" \
  --expires 1735689600 > alice.jwt
```

Configure relay:

```toml
[auth]
key = "root.jwk"
```

Connect with token:

```
https://relay.example.com/rooms/meeting-123?jwt=<token>
```

## Monitoring

### Metrics

The relay exposes metrics (Prometheus format planned):

- Connection count
- Bandwidth usage
- Track/broadcast count
- Error rates

### Logging

Structured logging via `tracing`:

```bash
# Set log level
RUST_LOG=info moq-relay --config relay.toml

# More verbose
RUST_LOG=debug moq-relay --config relay.toml

# Specific module
RUST_LOG=moq_relay=debug moq-relay --config relay.toml
```

### Health Checks

Check relay health:

```bash
# Basic connectivity (will fail without valid JWT)
curl https://relay.example.com/anon
```

Set up proper health monitoring with your provider's tools (CloudWatch, Stackdriver, etc.).

## Performance

### Current Status

- **Single-threaded** - Quinn uses one UDP receive thread
- **Mesh clustering** - All relays connect to all others
- **Memory buffering** - Recent groups cached in memory

### Scaling Tips

1. **Vertical scaling** - Fast CPU matters more than core count
2. **Regional deployment** - Reduce cross-region traffic
3. **Limit cluster size** - 3-5 relays optimal
4. **Monitor bandwidth** - Most important metric

### Future Improvements

Planned optimizations:
- Multi-threaded UDP processing
- Tree-based clustering topology
- Improved memory management

## Security

### Best Practices

1. **Always use TLS** - Required for WebTransport
2. **Enable authentication** - Use JWT tokens
3. **Limit public paths** - Minimize anonymous access
4. **Rotate keys** - Regularly update JWT keys
5. **Monitor traffic** - Watch for abuse
6. **Firewall rules** - Restrict to necessary ports

### DDoS Protection

Basic protection:

- Use cloud provider DDoS protection
- Rate limit connections (application level)
- Geographic filtering if needed
- Monitor bandwidth usage

## Troubleshooting

### Certificate Issues

```
Error: TLS handshake failed
```

Check:
- Certificate is valid and not expired
- Certificate matches domain name
- Private key permissions are correct
- Certificate includes full chain

### UDP Blocked

```
Error: Connection timeout
```

Check:
- UDP port 4443 is open in firewall
- Cloud provider allows UDP traffic
- No NAT/router blocking
- Try different port

### High Memory Usage

The relay caches recent groups in memory. If memory usage is high:

- Reduce cache duration (future config option)
- Increase available RAM
- Monitor for memory leaks (report if found)

## Next Steps

- Set up [Authentication](/guide/authentication)
- Understand [Architecture](/guide/architecture)
- Read [Protocol specifications](/guide/protocol)
- Build with [Rust](/rust/) or [TypeScript](/typescript/) libraries
