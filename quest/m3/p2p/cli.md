# [M] moq-cli joins the LAN

## Goal

`moq-cli --p2p` joins `.p2p/` on its relay session, advertises how it can be
reached, and accepts LAN browsers over data channels as a transit hop between
them and the relay. Native peers that both advertise iroh dial each other
directly over `iroh://`.

## Plan

`--p2p` (`MOQ_P2P`) needs a relay session (`--connect`) and a listener
(`--listen`); shape the flag so the value carries those prerequisites rather
than adding startup checks, per
[cluster discovery flags](/quest/m2/cluster-flags.md).

The roster entry mirrors the `info.json` schema from
[signaling](/quest/m3/p2p/signal.md): the listener's LAN addresses as
WebTransport URLs, ordered the way `mdns::Peer::urls` orders them, with the
listener's certificate fingerprint; the iroh endpoint id when `--iroh` is on;
and `webrtc: true`. The generated certificate must satisfy the browser's
`serverCertificateHashes` rules (ECDSA P-256, under fourteen days); verify the
generator does. Offers and answers go through the same per-pair broadcasts,
mirrored in Rust.

Every accepted browser session attaches to the shared origin with
`with_publisher` and `with_subscriber`, as the LAN mesh does, so the node is
a hop: it subscribes upstream once and serves each browser from the same
origin. Native pairs where both advertise iroh use the existing iroh client,
lower id dialing.

Docs: a "Web P2P" section in `doc/bin/cli.md` beside the LAN cluster one.

## Required

- [Native data channel transport](/quest/m3/p2p/webrtc.md)
- [Signaling over the relay](/quest/m3/p2p/signal.md) - the roster and offer schema this mirrors
