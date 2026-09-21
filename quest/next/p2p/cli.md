# [M] moq-cli joins

## Goal

`moq-cli --p2p` publishes a roster entry on its relay session, advertises how
it can be reached, dials other native peers over iroh when both advertise an
endpoint, and accepts browsers over data channels as a transit hop between
them and the relay. Its policy knobs mirror `@moq/p2p`.

## Plan

`--p2p` (`MOQ_P2P`) needs a relay session (`--connect`) and a listener
(`--listen`); shape the flag so the value carries those prerequisites rather
than adding startup checks. `--p2p-ice-server`,
`--p2p-max`, `--p2p-prefix`, and `--p2p-meta` carry the same knobs the JS
`Peers` takes; `select` is the CLI's built-in default of dialing everyone.

The roster entry mirrors the `info.json` schema from
[signaling](/quest/next/p2p/signal.md): the listener's LAN addresses as
WebTransport URLs, ordered the way `mdns::Peer::urls` orders them, with the
listener's certificate fingerprint; the iroh endpoint id when `--iroh` is on,
which is the first place an endpoint id appears in any roster; `webrtc: true`;
and `meta`. The generated certificate must satisfy the browser's
`serverCertificateHashes` rules (ECDSA P-256, under fourteen days); verify the
generator does. Offers and answers go through the same per-pair broadcasts,
mirrored in Rust.

Every accepted session attaches to the shared origin with `with_publisher`
and `with_subscriber`, as the LAN mesh does, so the node is a hop: it
subscribes upstream once and serves each peer from the same origin. Native
pairs where both advertise iroh use the existing iroh client, lower id
dialing, and skip ICE entirely.

Docs: a "P2P" section in `doc/bin/cli.md` beside the LAN cluster one.

## Required

- [Native data channel transport](/quest/next/p2p/webrtc.md)
- [Signaling and policy](/quest/next/p2p/signal.md) - the roster and offer schema this mirrors
