---
title: Production Setup
description: Setting up your production environment
---

# Production Setup

Here's a quick guide on how to get MoQ running in production.

## Relay
[moq-relay](/rust/moq-relay) is the core of the MoQ stack.
It's responsible for routing live tracks (payload agnostic) from 1 client to N clients.
The relay accepts WebTransport connections from clients, but it can also connect to other relays to fetch upstream.
Think of the relay as a HTTP web server like [Nginx](https://nginx.org/), but for live content.

There are multiple companies working on MoQ CDNs (like [Cloudflare](https://moq.dev/blog/first-cdn)) so eventually it won't be necessary to self-host.
However, you do unlock some powerful features by self-hosting, such as running relays within your internal network.

### QUIC Requirements
Before we get carried away, we need to cover the [QUIC](/guide/quic) requirements:

1. QUIC is a client-server protocol, so you **MUST** have a server with a static IP address.
2. QUIC requires TLS, so you **MUST** have a TLS certificate, even if it's self-signed.
3. QUIC uses UDP, so you **MUST** configure your firewall to allow UDP traffic.
4. QUIC load balancers don't exist yet, so you **MUST** design your own load balancer.

These make it a bit more difficult to deploy, but don't worry we have you covered.

### EZ Mode
`https://cdn.moq.dev` is a free, public MoQ relay.
Check out the [cdn](https://github.com/moq-dev/moq/tree/main/cdn) directory for the source code.

It consists of a relay server in the US, Europe, and Asia.
Clients use [GeoDNS](https://en.wikipedia.org/wiki/GeoDNS) to connect to the nearest relay, and relays connect to each other to form a global mesh.
You can also connect to individual nodes directly:
- `https://usc.cdn.moq.dev`
- `https://euc.cdn.moq.dev`
- `https://sea.cdn.moq.dev`

Here's a quick tl;dr of the setup:
- [Linode](https://linode.com/) is the VM provider.
- [GCP](https://cloud.google.com/) is the DNS provider.
- [OpenTofu](https://opentofu.org/) (aka Terraform) sets up the infrastructure.
- [Nix](https://nixos.org/) is used to build/cache the binaries.
- [systemd](https://systemd.io/) runs the services.
- [ssh](https://en.wikipedia.org/wiki/Secure_Shell) + [rsync](https://en.wikipedia.org/wiki/Rsync) are used to deploy.
- [certbot](https://certbot.eff.org/) + [Let's Encrypt](https://letsencrypt.org/) procures TLS certificates.

Any EC2-like cloud provider will work; we just need a VM with a public IP address.
The old setup used to use [GCP](https://cloud.google.com/) but was double the cost.
We're still using [GCP](https://cloud.google.com/blog/products/networking/dns-routing-policies-for-geo-location--weighted-round-robin) for GeoDNS because there aren't many other options and it's virtually free (unlike Cloudflare).

Read the [moq-relay](/rust/moq-relay) documentation for more details on how to configure the relay server.

### Hard Mode
If you don't want to use the EZ Mode, don't worry you can build your own stack.
MoQ should work just fine inside your own network or infrastructure provided you understand the QUIC requirements.

You need at least one server with some way to discover its IP address.
DNS is the easiest way to do this, but some other way of getting an IP address should also work.
QUIC also has really awesome anycast support but that's a bit more advanced; reach out if you're interested.

TLS is where most people get stuck.
[See my blog post](https://moq.dev/blog/tls-and-quic) for more details, but here's the important bits:

- QUIC uses the same TLS certificate as HTTPS.
- However, TLS load balancers currently don't support QUIC, so you need to provision your own TLS certificates.
-  You can disable TLS verification if you don't care about MITM attacks, but only for native clients.
-  Web browsers can support self-signed certificates via [fingerprint verification](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#servercertificatehashes), but it's limited to ephemeral certificates (<2 weeks).

And of course, make sure UDP is allowed on your firewall.
The default WebTransport port is UDP/443 but anything will work if you put it in the URL.

## Web
Whew, the hard part is over.
The web client is pretty standard:

- [@moq/lite](/typescript/moq): generic network transport
- [@moq/hang](/typescript/hang): media library and WebComponents
- [@moq/hang-ui](/typescript/hang-ui): optional UI components

Currently, you need to use a bundler and [Vite](https://vite.dev/) is the only supported option for `@moq/hang`.
It makes me very sad and we're working on a more universal solution, contributions welcome!

**NOTE** both of these libraries are intended for client-side.
However, `@moq/lite` can run on the server side using a [WebTransport polyfill](https://github.com/fails-components/webtransport).
Don't even try to run `@moq/hang` on the server side or you'll run into a ton of issues, *especially* with Next.js.

## Native
Native clients are the easiest to get running, but also the most limited.

We have a few integrations with popular libraries:
- [obs](/rust/obs): OBS Studio plugin for publishing or consuming media
- [gstreamer](/rust/gstreamer): GStreamer plugin for publishing or consuming media
- [hang-cli](/rust/hang-cli): command-line tool for publishing media (from stdin/ffmpeg)

Or you can use the core libraries directly:
- [moq-lite](/rust/moq-lite): generic network transport
- [hang](/rust/hang): media library (no encoding/decoding support yet)
- [libmoq](/rust/libmoq): C bindings for [moq-lite](/rust/moq-lite) and [hang](/rust/hang)

You just need to make sure UDP is allowed on your firewall.
If the server is using a non-standard TLS certificate, you'll need to configure the client to accept it.

## What's Next?
Grats on getting MoQ running in production!
I knew you could do it.

Check out the [concepts](/concepts/) and [API](/api/) docs to dive deeper.
