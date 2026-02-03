---
title: Configuration
description: TOML configuration reference for moq-relay
---

# Configuration

moq-relay is configured via a TOML file. Pass the path as the only argument:

```bash
moq-relay relay.toml
# or
moq-relay --config relay.toml
```

## Minimal Example

```toml
[server]
listen = "0.0.0.0:4443"

[server.tls]
cert = "cert.pem"
key = "key.pem"
```

## Full Reference

### [log]

Logging configuration.

```toml
[log]
# Log level: trace, debug, info, warn, error
# The RUST_LOG environment variable takes precedence
level = "info"
```

### [server]

QUIC/WebTransport server settings.

```toml
[server]
# Listen address for QUIC (UDP)
listen = "0.0.0.0:4443"
```

### [server.tls]

TLS configuration for the QUIC endpoint.

```toml
[server.tls]
# Option 1: Provide certificate files
cert = "/path/to/cert.pem"   # Certificate chain
key = "/path/to/key.pem"     # Private key

# Option 2: Generate self-signed certificates (development only)
generate = ["localhost", "127.0.0.1"]
```

For production, use certificates from Let's Encrypt or another CA.

### [web.http]

HTTP server for debugging endpoints.

```toml
[web.http]
# Listen address for HTTP (TCP)
# Defaults to disabled if not specified
listen = "0.0.0.0:4443"
```

See [HTTP Endpoints](/app/relay/http) for available endpoints.

### [web.https]

HTTPS/WSS server for TCP fallback.

```toml
[web.https]
# Listen address for HTTPS/WSS (TCP)
listen = "0.0.0.0:443"

# TLS certificates (can be the same as server.tls)
cert = "cert.pem"
key = "key.pem"
```

### [auth]

Authentication configuration.

```toml
[auth]
# Path to the JWT verification key
# - Symmetric: the shared secret key
# - Asymmetric: the public key
key = "root.jwk"

# Path prefix for anonymous access
# Omit to require authentication everywhere
public = "anon"
```

See [Authentication](/app/relay/auth) for details on token generation.

### [cluster]

Clustering configuration for multi-relay deployments.

```toml
[cluster]
# Address of the root relay to connect to
# Omit this to make this relay the root
connect = "root.relay.example.com:4443"

# JWT token file for cluster authentication
token = "cluster.jwt"

# This relay's address, as reachable by other cluster nodes
node = "leaf1.relay.example.com:4443"
```

See [Clustering](/app/relay/cluster) for deployment patterns.

### [client]

Client settings used when connecting to other relays (clustering).

```toml
[client]
# Disable TLS verification (development only!)
tls.disable_verify = true

# Or provide trusted root certificates
# tls.root = ["/path/to/root.pem"]
```

### [iroh]

Experimental P2P support via iroh.

```toml
[iroh]
# Enable iroh for P2P connections
enabled = false

# Path to persist the iroh secret key
secret = "./relay-iroh-secret.key"
```

## Example Configurations

### Development

```toml
[log]
level = "debug"

[server]
listen = "0.0.0.0:4443"
tls.generate = ["localhost"]

[web.http]
listen = "0.0.0.0:4443"

[auth]
public = ""  # No authentication
```

### Production

```toml
[server]
listen = "0.0.0.0:443"

[server.tls]
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"

[web.https]
listen = "0.0.0.0:443"
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"

[auth]
key = "public.jwk"
```

### Cluster Leaf Node

```toml
[server]
listen = "0.0.0.0:443"

[server.tls]
cert = "cert.pem"
key = "key.pem"

[cluster]
connect = "root.relay.example.com:443"
token = "cluster.jwt"
node = "leaf.relay.example.com:443"

[client]
# Use proper TLS verification in production
# tls.root = ["root-ca.pem"]

[auth]
key = "public.jwk"
public = "anon"
```

## Environment Variables

- `RUST_LOG` - Override the log level (e.g., `RUST_LOG=debug`)
- `MOQ_IROH_SECRET` - Set the iroh secret key directly

## See Also

- [Authentication](/app/relay/auth) - JWT setup
- [HTTP Endpoints](/app/relay/http) - Debug endpoints
- [Clustering](/app/relay/cluster) - Multi-relay deployments
- [Production Deployment](/setup/prod) - Production checklist
