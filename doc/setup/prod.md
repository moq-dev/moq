---
title: Production Deployment
description: Run moq-relay publicly with TLS, authentication, and host tuning
---

# Production Deployment

A public relay needs a reachable UDP port, a trusted certificate, and an
explicit access policy. Start from the local config and change these:

1. Give the relay a stable hostname and forward its UDP port (QUIC and WebTransport). Forward TCP too if you enable HTTPS or the WebSocket fallback.
2. Get a publicly trusted certificate, ideally from the relay's own `[acme]` client. Generated certificates and disabled verification are for development.
3. Configure [authentication](/bin/relay/auth). Leave nothing anonymous unless you mean to.
4. Keep the operational endpoints (`/metrics`, `/nodes`) on the `[internal]` listener, bound to loopback or a private network.
5. Raise Linux UDP socket buffers (below).

```toml
[listen]
bind = "[::]:443"

# HTTPS and WebSocket fallback on TCP, which also answers the ACME challenge.
[web.https]
listen = "[::]:443"

# A Let's Encrypt certificate for QUIC and HTTPS, renewed in the background.
[acme]
domains = ["relay.example.com"]
contact = ["mailto:ops@example.com"]
cache = "/var/lib/moq-relay/acme"

[auth]
url = "http://127.0.0.1:4440/"

[internal]
listen = "127.0.0.1:9101"
```

Run the auth server on the same host and supervise it alongside the relay:

```bash
moq auth serve --key /etc/moq/public.jwk
```

It verifies each session's JWT; a session without one is refused. See
[Authentication](/bin/relay/auth) to define another access policy.

Let's Encrypt validates the domain by connecting to TCP port 443, so
`web.https.listen` must be reachable there, directly or forwarded. The first
start waits for the certificate before serving anything; later starts reuse
the one kept in `dir`. Test against the staging directory first, since
production rate-limits failed attempts. See [`[acme]`](/bin/relay/config#acme).

To bring your own certificate instead (certbot, cert-manager, a private CA),
drop `[acme]` and point `listen.tls` and `web.https` at the files:

```toml
[listen.tls]
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"

[web.https]
listen = "[::]:443"
cert = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
key = "/etc/letsencrypt/live/relay.example.com/privkey.pem"
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
