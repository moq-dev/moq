---
title: Production Deployment
description: Deploy moq-relay with production networking, TLS, and authentication
---

# Production Deployment

A production relay needs a reachable QUIC endpoint, a trusted TLS certificate,
and an explicit access policy. Start with a working local configuration, then
make each of these changes before exposing it publicly.

## Deployment checklist

1. Assign the relay a stable hostname.
2. Route the configured UDP port to the relay for QUIC and WebTransport.
3. Route TCP if the relay also serves HTTPS, WebSocket, or HTTP endpoints.
4. Install a certificate whose names include the public hostname.
5. Configure [authentication](/bin/relay/auth); do not leave the entire path
   tree anonymous unless that is intentional.
6. Bind operational endpoints only where trusted monitoring systems can reach
   them.
7. Raise UDP socket limits on Linux and monitor startup warnings.

The [relay configuration reference](/bin/relay/config) documents the relevant
`server`, `server.tls`, `web`, `internal`, and `auth` sections.

## Networking and TLS

QUIC uses UDP and performs TLS in the relay process. Network infrastructure must
forward UDP to a relay that can terminate the QUIC connection. If HTTPS or the
WebSocket fallback is enabled, forward the corresponding TCP port as well.

Use a certificate from a CA trusted by your clients. QUIC and HTTPS/WSS have
separate TLS sections, although they can reuse the same certificate files:

```toml
[server.tls]
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"

[web.https]
listen = "[::]:443"
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"
```

Generated certificates and disabled verification are intended for development.

Browser clients can use certificate fingerprint verification for short-lived
self-signed development certificates. Native clients can use custom root CAs,
but a public service should normally use a publicly trusted certificate.

## Access and topology

Use JWT authentication to scope which paths a client can publish or subscribe
to. Keep operational HTTP endpoints on the `internal` listener when they should
not be public.

For multiple regions or failure domains, connect relays with the
[clustering configuration](/bin/relay/cluster). Each relay should have a stable
external URL and a topology chosen for the deployment. Clustering complements
load distribution; it does not decide how clients discover or select an entry
relay.

## Linux socket buffers

A relay multiplexes connections over UDP sockets. If an incoming burst exceeds
the kernel socket buffer, packets are dropped before the relay can process them.

`moq-relay` requests an 8 MiB buffer in each direction. Linux may clamp that
request to `net.core.rmem_max` and `net.core.wmem_max`; the relay logs a warning
when the granted size is too small. Raise both limits and persist them across
reboots:

```bash
printf 'net.core.rmem_max = 8388608\nnet.core.wmem_max = 8388608\n' | \
  sudo tee /etc/sysctl.d/60-moq.conf
sudo sysctl --system
```

macOS uses `kern.ipc.maxsockbuf`. Windows sizes each socket directly and does
not use these Linux limits.

## Verify the deployment

- Connect with [moq-cli](/bin/cli) from outside the relay network.
- Publish and subscribe through the paths allowed by a test token.
- Check the [health and metrics endpoints](/bin/relay/http).
- Confirm startup logs show the expected certificate, listeners, and socket
  buffer sizes.
