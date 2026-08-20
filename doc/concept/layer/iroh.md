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
[`moq-tokio`](/lib/rs/crate/moq-tokio).
:::

## Why

The usual MoQ deployment is a [relay](/bin/relay/) with a public IP, a DNS name, and a TLS
certificate. That's the right answer at scale, but it's a lot of ceremony when:

- Two machines on the same LAN want to exchange media and neither is a server.
- You want a stable identity for a node whose IP address keeps changing.
- You don't want to own a domain or provision a certificate just to move some frames.

iroh solves all three with the same mechanism: the endpoint's key **is** its address.

## Scope

Peer-to-peer is a **local network** feature. Keep it there.

- **On one network**, peers dial each other directly, and with `--iroh-disable-relay` the
  media never leaves the link. That's the case iroh is for here. (The addressing leaves
  either way; see [Connectivity](#connectivity).)
- **Across the internet**, run a [MoQ relay](/bin/relay/). It has an address both sides can
  already reach, it caches and fans out, and it's the only shape that scales past two peers.

The iroh relay is the part to be careful with. By default your packets go through an n0
server from the first byte, and stay there for good if the hole punch never lands. That's the
same shape as WebRTC's TURN: a third party in the media path that has no idea what it's
carrying, can't cache a group, can't serve the second viewer from the first one's fetch, and
adds however many milliseconds its location costs. If media has to cross a server anyway,
that server should be a MoQ relay.

`--iroh-disable-relay` takes that off the table, but read [Connectivity](#connectivity)
before reaching for it: an iroh relay is not only the fallback path, it is also how two peers
coordinate a hole punch and how an endpoint learns its own public address. Turning it off
leaves direct addresses and nothing else, which is exactly right on one link and not much use
across the internet. That is the trade this page is recommending, not a way to keep NAT
traversal while dropping the forwarding.

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
2. **iroh relay** carries the connection immediately, so there's no connect-time stall.
3. **Hole punching** then tries to open a direct path, and the connection migrates onto it.

The relay does more than carry the traffic. It's the rendezvous both sides reach first, so
it's also how they coordinate the punch, and its probes are how an endpoint learns the public
address to advertise in the first place. Only once the direct path is up does it drop out,
and it stays in the path as a fallback if the punch never succeeds.

So `--iroh-disable-relay` is not "punch, but don't fall back". It removes stages 2 and 3
together and leaves stage 1: a peer is reachable if its advertised addresses reach you, and
unreachable otherwise. On one link that's all you need, and it's the [recommended](#scope)
setting there. Across a NAT it mostly means no connection, and an endpoint that can't see its
own public address advertises only its local ones.

**Discovery runs either way.** An endpoint publishes its own addressing record to n0's
`iroh.link` DNS server whenever its addresses change, so turning iroh on tells a third party
which endpoint ids are live and where they are, whatever the relay setting. Disabling the
relay doesn't opt out; it only changes what the record holds, since with no relay URL to
publish it carries direct IP addresses instead. Resolution is the half you can avoid:
`Client::with_iroh_addrs` seeds a peer's addresses so a LAN dial needs no lookup. Publishing
has no such escape hatch.

Weigh that before enabling iroh for something that has to stay local. The media can be pinned
to a direct path, the addressing can't.

::: tip Two different things called "relay"
An **iroh relay** forwards opaque UDP packets between two peers that can't reach each
other. It doesn't speak MoQ and doesn't know a track from a group.
A [**MoQ relay**](/bin/relay/) is a CDN node that understands broadcasts, fans them out,
and caches groups.
They're unrelated, and when media has to cross a server, it should be the second one.
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

Whether to add `disable_relay` depends on where this relay's iroh clients are, and the
[relay config reference](/bin/relay/config#iroh) has the trade. It keeps n0 out of the media
path, and it breaks a relay that clients dial over `iroh://` from anywhere but its own
network.

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
# Publish to a peer on this network.
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq --iroh-enabled --iroh-disable-relay \
        --connect "iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa/anon" \
        --broadcast my-stream.hang import ts

# Play it back from another machine on the same network.
moq --iroh-enabled --iroh-disable-relay \
    --connect "iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa/anon" \
    --broadcast my-stream.hang play
```

Drop `--iroh-disable-relay` and the two machines can be anywhere, at the cost of an n0 relay
in the media path until a punch succeeds. Prefer a [MoQ relay](/bin/relay/) for that case.

Dialing an `iroh://` URL without `--iroh-enabled` is an error, not a silent fallback.

| Flag | Environment | Purpose |
|---|---|---|
| `--iroh-enabled` | `MOQ_IROH_ENABLED` | Bind the endpoint. Required for `iroh://`. |
| `--iroh-secret` | `MOQ_IROH_SECRET` | Hex key or a path to persist one. |
| `--iroh-bind-v4` | `MOQ_IROH_BIND_V4` | UDP bind address, default `0.0.0.0:0`. |
| `--iroh-bind-v6` | `MOQ_IROH_BIND_V6` | UDP bind address, default `[::]:0`. |
| `--iroh-disable-relay` | `MOQ_IROH_DISABLE_RELAY` | Direct addresses only: no fallback, and no hole punching. Recommended on a LAN. |

The endpoint is shared by the client and server halves of a process, so one set of flags
covers both dialing out and accepting in.

## Building

iroh is behind the `iroh` cargo feature.
It's on by default for `moq-cli` and `moq-relay`, and off by default for
[`moq-tokio`](/lib/rs/crate/moq-tokio):

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
- **Congestion control defaults to CUBIC**, unlike the quinn and quiche backends, which
  default to BBR. iroh shares noq's BBRv3, which can underflow and panic on a packet loss,
  so delay-based control stays reachable only when an operator asks for it by name. The
  noq backend defaults to CUBIC for the same reason. See the
  [relay config](/bin/relay/config).
- **GSO can't be disabled.** `--client-quic-gso=false` is refused rather than ignored.
- **No keep-alive knob.** Stream limits, idle timeout, MTU discovery, and congestion
  control carry over; nothing else does.
- **Publishing is not local.** An endpoint's addressing record goes to n0's servers whatever
  the relay setting, as above. Resolution can be skipped, publishing can't.
- **No browser support**, as above.

## More Info

- [QUIC](/concept/layer/quic) - what iroh is carrying underneath.
- [Relay configuration](/bin/relay/config#iroh) - the `[iroh]` section.
- [CLI](/bin/cli) - publishing and playing over `iroh://`.
