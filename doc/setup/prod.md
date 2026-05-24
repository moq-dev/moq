---
title: Production Deployment
description: Deploying moq-relay to production
---

# Production Deployment

Here's a guide on how to get moq-relay running in production.

## Overview

[moq-relay](/app/relay/) is the core of the MoQ stack.
It's responsible for routing live tracks (payload agnostic) from 1 client to N clients.
The relay accepts WebTransport connections from clients, but it can also connect to other relays to fetch upstream.
Think of the relay as a HTTP web server like [Nginx](https://nginx.org/), but for live content.

There are multiple companies working on MoQ CDNs (like [Cloudflare](https://moq.dev/blog/first-cdn)) so eventually it won't be necessary to self-host.
However, you do unlock some powerful features by self-hosting, such as running relays within your internal network.

## QUIC Requirements

Before we get carried away, we need to cover the QUIC requirements:

1. QUIC is a client-server protocol, so you **MUST** have a server with a static IP address.
2. QUIC requires TLS, so you **MUST** have a TLS certificate, even if it's self-signed.
3. QUIC uses UDP, so you **MUST** configure your firewall to allow UDP traffic.
4. QUIC load balancers don't exist yet, so you **MUST** design your own load balancer.

These make it a bit more difficult to deploy, but don't worry we have you covered.

## Self-Hosting

MoQ should work just fine inside your own network or infrastructure provided you understand the QUIC requirements.

You need at least one server with some way to discover its IP address.
DNS is the easiest way to do this, but some other way of getting an IP address should also work.
QUIC also has really awesome anycast support but that's a bit more advanced; reach out if you're interested.

TLS is where most people get stuck.
[See my blog post](https://moq.dev/blog/tls-and-quic) for more details, but here's the important bits:

- QUIC uses the same TLS certificate as HTTPS.
- However, TLS load balancers currently don't support QUIC, so you need to provision your own TLS certificates.
- You can disable TLS verification if you don't care about MITM attacks, but only for native clients.
- Web browsers can support self-signed certificates via [fingerprint verification](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#servercertificatehashes), but it's limited to ephemeral certificates (<2 weeks).

And of course, make sure UDP is allowed on your firewall.
The default WebTransport port is UDP/443 but anything will work if you put it in the URL.

## Next Steps

- Set up [Authentication](/app/relay/auth)
- Configure [Clustering](/app/relay/cluster)
- Learn about [Concepts](/concept/)
