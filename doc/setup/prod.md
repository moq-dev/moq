---
title: Production Deployment
description: Run moq-relay publicly with TLS, authentication, and host tuning
---

# Production Deployment

A public relay needs a reachable UDP port, a trusted certificate, and an
explicit access policy. Start from the local config and change these:

1. Give the relay a stable hostname and forward its UDP port (QUIC and WebTransport). Forward TCP too if you enable HTTPS or the WebSocket fallback.
2. Install a publicly trusted certificate. Generated certificates and disabled verification are for development.
3. Configure [authentication](/bin/relay/auth). Leave nothing anonymous unless you mean to.
4. Keep the operational endpoints (`/metrics`, `/nodes`) on the `[internal]` listener, bound to loopback or a private network.
5. Raise Linux UDP socket buffers (below).

```toml
[server]
bind = "[::]:443"

[server.tls]
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"

# HTTPS and WebSocket fallback on TCP. The same certificate works.
[web.https]
listen = "[::]:443"
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"

[auth]
key = "/etc/moq/public.jwk"

[internal]
listen = "127.0.0.1:9101"
```

Certificate files are watched and reloaded for new connections. See the
[configuration reference](/bin/relay/config) for every section.

## Socket buffers

The relay asks for 8 MiB UDP buffers and logs a warning when the kernel clamps
them. On Linux, raise the limits and persist them:

```bash
printf 'net.core.rmem_max = 8388608\nnet.core.wmem_max = 8388608\n' | sudo tee /etc/sysctl.d/60-moq.conf
sudo sysctl --system
```

macOS uses `kern.ipc.maxsockbuf`; Windows sizes each socket directly.

## Scaling out

Connect relays into a [cluster](/bin/relay/cluster) to serve multiple regions
or add redundancy. Clustering routes broadcasts between relays; how clients
pick an entry relay (DNS, anycast, a load balancer) is up to you. `/health`
is the liveness probe.

If you would rather not run infrastructure, [moq.pro](https://moq.pro) hosts
relays behind an API.

## Verify

- Connect with [moq-cli](/bin/cli) from outside the network and publish with a test token.
- Watch it with the [web player](/lib/js/watch) or `moq play`.
- Check [`/health` and `/metrics`](/bin/relay/http), and confirm the startup log shows the expected listeners, certificate, and buffer sizes.
