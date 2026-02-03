---
title: HTTP Endpoints
description: Debug endpoints exposed by moq-relay
---

# HTTP Endpoints

moq-relay exposes HTTP endpoints for debugging and diagnostics. These run on TCP alongside the QUIC/UDP server.

::: warning
The HTTP server is unencrypted and intended for local debugging only. Don't expose it to the public internet.
:::

## Configuration

```toml
[web.http]
# Listen for HTTP connections on TCP
listen = "0.0.0.0:4443"
```

The default is the same port as the QUIC server, just on TCP instead of UDP.

## Endpoints

### GET /certificate.sha256

Returns the SHA-256 fingerprint of the TLS certificate. Useful for local development with self-signed certificates.

```bash
curl http://localhost:4443/certificate.sha256
# f4:a3:b2:... (hex-encoded fingerprint)
```

Browsers can use this fingerprint to trust the self-signed certificate for WebTransport connections.

### GET /announced/*prefix

Lists all announced broadcasts matching the given prefix.

```bash
# All broadcasts
curl http://localhost:4443/announced/

# Broadcasts under "demo/"
curl http://localhost:4443/announced/demo

# Specific broadcast
curl http://localhost:4443/announced/demo/my-stream
```

Returns a list of broadcast paths currently available on the relay.

### GET /fetch/*path

Fetches the latest group from a track. Useful for quick debugging without setting up a full subscriber.

```bash
# Get latest video group
curl http://localhost:4443/fetch/demo/my-stream/video

# Get latest audio group
curl http://localhost:4443/fetch/demo/my-stream/audio
```

Returns the raw bytes of the most recent group on that track.

## Use Cases

### Local Development

During development, use `/certificate.sha256` to get the fingerprint of auto-generated certificates:

```bash
# Start the relay
moq-relay dev/relay.toml

# Get the fingerprint for WebTransport
FINGERPRINT=$(curl -s http://localhost:4443/certificate.sha256)
```

### Debugging

Check what's being announced:

```bash
# Is my publisher connected?
curl http://localhost:4443/announced/my-stream

# What's available under this prefix?
curl http://localhost:4443/announced/demo
```

Peek at actual data:

```bash
# Is the video track producing data?
curl http://localhost:4443/fetch/demo/stream/video | hexdump -C | head
```

### Health Checks

The HTTP endpoints can serve as basic health checks:

```bash
# Is the relay responding?
curl -f http://localhost:4443/announced/ || echo "Relay down"
```

## See Also

- [Relay Configuration](/app/relay/config) - Full config reference
- [Clustering](/app/relay/cluster) - Multi-relay deployments
