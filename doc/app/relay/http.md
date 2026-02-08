---
title: HTTP
description: HTTP endpoints exposed by moq-relay
---

# HTTP Endpoints

moq-relay exposes HTTP/HTTPS endpoints via TCP too.
These were initially added for debugging but are useful for many things, such as fetching old content.

## Configuration

The relay supports both HTTP and HTTPS, configured independently:

```toml
[web.http]
# Listen for unencrypted HTTP connections on TCP
listen = "0.0.0.0:80"

[web.https]
# Listen for encrypted HTTPS connections on TCP
listen = "0.0.0.0:443"
cert = "cert.pem"
key = "key.pem"
```

::: warning
HTTP is unencrypted, which means any [authentication tokens](/app/relay/auth) will be sent in plaintext.
It's recommended to only use HTTPS in production.
:::

## Notable Endpoints

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

### GET /fetch/*path

Fetches a specific group from a track, by default the latest group.
Useful for quick debugging without setting up a full subscriber, or for fetching old content.

The path is `/<broadcast>/<track>`, where the last segment is the track name and everything before it is the broadcast path.

```bash
# Get latest catalog from broadcast "demo/my-stream"
curl http://localhost:4443/fetch/demo/my-stream/catalog.json

# Get a specific video group from broadcast "demo/my-stream"
curl http://localhost:4443/fetch/demo/my-stream/video?group=42
```

::: tip
Use HTTP fetch for catch-up and historical data.
Use MoQ subscriptions for the live edge.
The two complement each other — HTTP is request/response, MoQ is pub/sub.
:::

### GET /certificate.sha256

Returns the SHA-256 fingerprint of the TLS certificate.
This is only useful for local development with self-signed certificates.

```bash
curl http://localhost:4443/certificate.sha256
# f4:a3:b2:... (hex-encoded fingerprint)
```


## See Also

- [Relay Configuration](/app/relay/config) - Full config reference
- [Clustering](/app/relay/cluster) - Multi-relay deployments
- [hang format](/concept/layer/hang) - Groups, keyframes, and container details
