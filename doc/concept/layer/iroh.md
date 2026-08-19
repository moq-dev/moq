---
title: iroh
description: Peer-to-peer QUIC, dialed by public key instead of a hostname.
---

# iroh

[iroh](https://www.iroh.computer/) is an optional transport that swaps the "client dials a
server" model for "peer dials a peer".
It's still QUIC underneath, so every MoQ layer above it is unchanged.
What changes is *addressing* and *connectivity*: you dial a public key instead of a hostname,
and iroh does the NAT traversal to reach it.

::: warning
This is experimental and **native only**.
Browsers can't do this. WebTransport gives a page a QUIC connection, not a QUIC endpoint,
so there's no hole punching and no way to accept an inbound connection.
Everything on this page applies to `moq-relay`, `moq`, and anything else built on
[`moq-native`](/lib/rs/crate/moq-native).
:::

## Why

The usual MoQ deployment is a [relay](/bin/relay/) with a public IP, a DNS name, and a TLS
certificate. That's the right answer at scale, but it's a lot of ceremony when:

- Two machines on the same LAN want to exchange media and neither is a server.
- A laptop behind a NAT wants to publish, and you'd rather not run a relay in the middle.
- You want a stable identity for a node whose IP address keeps changing.
- You don't want to own a domain or provision a certificate just to move some frames.

iroh solves all four with the same mechanism: the endpoint's key **is** its address.

## Addressing

An iroh endpoint is an ed25519 keypair.
The public half is the **endpoint id**, and that's what you dial:

```text
iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa/room?jwt=...
```

The host is the endpoint id, not a hostname. The path and query mean exactly what they
mean on an `https://` URL: the [auth](/bin/relay/auth) path and an optional token.

TLS still runs, but there's no certificate authority in the picture.
The peer proves it holds the private key matching the id you dialed, which is the
whole point: you already named the key, so nothing else needs to vouch for it.
Dial the wrong key and the handshake fails. There's no such thing as a misissued
certificate for an endpoint id.

Note that the key is also the *only* identity.
iroh exposes no client certificate, so mTLS auth isn't available over this transport.
Authentication works like any other tokenized connection: a JWT in the URL, or anonymous
access to a public path.

## Connectivity

A single iroh endpoint plays both roles. The same bound socket dials out and accepts in,
which is what makes a true peer-to-peer topology possible.

Getting a packet to a peer behind a NAT happens in stages:

1. **Discovery** finds the peer's candidate addresses from its endpoint id.
2. **Hole punching** tries to establish a direct path between the two hosts.
3. **iroh relay** carries the traffic when a direct path can't be established.

The connection starts over the iroh relay and upgrades to a direct path once hole punching
succeeds, so there's no connect-time stall while the two sides negotiate.
By default this uses the public discovery and relay servers operated by
[n0](https://n0.computer/). Pass `--iroh-disable-relay` to require a direct path and skip
the fallback entirely.

::: tip Two different things called "relay"
An **iroh relay** forwards opaque UDP packets between two peers that can't reach each
other. It doesn't speak MoQ and doesn't know a track from a group.
A [**MoQ relay**](/bin/relay/) is a CDN node that understands broadcasts, fans them out,
and caches groups.
They're unrelated, and you can use either, both, or neither.
:::

## Binding

iroh connections negotiate their MoQ binding by ALPN, not by URL scheme.
The endpoint offers the moq-lite ALPNs first and `h3` last, so:

- **Two MoQ endpoints** land on **raw QUIC**, skipping HTTP/3 entirely. There's no CONNECT
  request to carry the request URI, so the path and token ride the moq-lite SETUP instead.
- **An H3 peer** falls back to **WebTransport**, where the path travels in the CONNECT URL
  like it does over `https://`.

You don't pick between them, and in practice you don't need to care.
It's worth knowing because it's why an `iroh://` URL's path can end up in either place,
and why the [`transport`](/bin/relay/auth) reported to the auth API is `iroh` rather than
`quic`.

## Usage

Enable the endpoint on both sides. It's off by default everywhere.

A relay opts in through its [config file](/bin/relay/config#iroh):

```toml
[iroh]
enabled = true

# Persist the key so the endpoint id survives a restart.
# Generated on first run if the file doesn't exist.
secret = "./iroh-secret.key"
```

Without `secret` the relay generates a fresh key on every start, which means a new
endpoint id and a stale URL for everyone who wrote the old one down.
`MOQ_IROH_SECRET` sets the key directly if a file doesn't suit your deployment.

Both the relay and the [CLI](/bin/cli) log the endpoint id at startup:

```text
INFO iroh listening endpoint_id=k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa
```

The CLI takes the same settings as flags. `--iroh-enabled` binds the endpoint, and then an
`iroh://` URL works anywhere an `https://` one would:

```bash
# Publish to a peer.
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq --iroh-enabled \
        --client-connect "iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa/anon" \
        --broadcast my-stream.hang import ts

# Play it back from another machine.
moq --iroh-enabled \
    --client-connect "iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa/anon" \
    --broadcast my-stream.hang play
```

Dialing an `iroh://` URL without `--iroh-enabled` is an error, not a silent fallback.

| Flag | Environment | Purpose |
|---|---|---|
| `--iroh-enabled` | `MOQ_IROH_ENABLED` | Bind the endpoint. Required for `iroh://`. |
| `--iroh-secret` | `MOQ_IROH_SECRET` | Hex key or a path to persist one. |
| `--iroh-bind-v4` | `MOQ_IROH_BIND_V4` | UDP bind address, default `0.0.0.0:0`. |
| `--iroh-bind-v6` | `MOQ_IROH_BIND_V6` | UDP bind address, default `[::]:0`. |
| `--iroh-disable-relay` | `MOQ_IROH_DISABLE_RELAY` | Direct paths only, no relay fallback. |

The endpoint is shared by the client and server halves of a process, so one set of flags
covers both dialing out and accepting in.

## Building

iroh is behind the `iroh` cargo feature.
It's on by default for `moq-cli` and `moq-relay`, and off by default for
[`moq-native`](/lib/rs/crate/moq-native):

```bash
cargo install moq-cli --no-default-features --features "iroh,quinn,websocket"
```

Library users bind the endpoint once and hand it to both halves:

```rust
let endpoint = iroh_config.bind(&client_config.quic).await?.unwrap();
let client = client_config.init()?.with_iroh(endpoint.clone());
let server = server_config.init()?.with_iroh(endpoint);
```

`Client::with_iroh_addrs` seeds known socket addresses for a peer, letting a LAN dial skip
discovery.

## Limitations

iroh runs on [noq](https://crates.io/crates/noq) rather than quinn, and it owns its own
transport config, so a few of the shared QUIC knobs behave differently:

- **The client QUIC settings apply to both roles.** One endpoint serves both, and the
  per-connection knobs are symmetric, so `[client.quic]` (`--client-quic-*`) is what it
  reads. The `[server.quic]` section doesn't reach it.
- **Congestion control defaults to CUBIC**, unlike every other backend, which defaults to
  BBR. noq's BBRv3 can underflow and panic on a packet loss, so delay-based control stays
  reachable only when an operator asks for it by name. See the
  [relay config](/bin/relay/config).
- **GSO can't be disabled.** `--client-quic-gso=false` is refused rather than ignored.
- **No keep-alive knob.** Stream limits, idle timeout, MTU discovery, and congestion
  control carry over; nothing else does.
- **No browser support**, as above.

## More Info

- [QUIC](/concept/layer/quic) - what iroh is carrying underneath.
- [Relay configuration](/bin/relay/config#iroh) - the `[iroh]` section.
- [CLI](/bin/cli) - publishing and playing over `iroh://`.
