# [M] moq-cli joins

## Goal

`moq-cli --p2p` publishes a roster entry on its relay session, advertises how
it can be reached, dials other native peers over
[ICE and QUIC](/quest/m2/p2p/native-quic.md), and accepts browsers over data channels as a transit hop between
them and the relay. Its policy knobs mirror `@moq/p2p`.

## Plan

`--p2p` (`MOQ_P2P`) needs a relay session (`--connect`) and a listener
(`--listen`); shape the flag so the value carries those prerequisites rather
than adding startup checks. `--p2p-ice-server`,
`--p2p-max`, `--p2p-prefix`, and `--p2p-meta` carry the same knobs the JS
`Peers` takes. Decided 2026-10-13: the operator explicitly selects peers;
there is no built-in dial-all default. Provide an explicit selection/configuration
surface and per-connection priority/cost mirroring the application API; settle
its concrete CLI shape before coding and update all invocations against --help.

The roster entry mirrors the `info.json` schema from
[signaling](/quest/m2/p2p/signal.md): the listener's LAN addresses as
WebTransport URLs, ordered the way `mdns::Peer::urls` orders them, with the
listener's certificate fingerprint. Use signaling's ordered-list shape and
dialing order, confirmed in the quest audit, so multi-interface listeners
advertise one shared schema. Include `webrtc: true`, the native ICE+QUIC
field, and `meta`. No iroh endpoint id (decided 2026-10-13: P2P does not use
iroh). The generated certificate must satisfy the browser's
`serverCertificateHashes` rules (ECDSA P-256, under fourteen days); verify the
generator does. Offers and answers go through the same per-pair broadcasts,
mirrored in Rust.

Every accepted session attaches to the shared origin with `with_publisher`
and `with_subscriber`, as the LAN mesh does, so the node is a hop: it
subscribes upstream once and serves each peer from the same origin. Native
pairs selected by the operator connect over ICE and QUIC, with
deterministic simultaneous-dial resolution.
Discovery alone does not initiate a connection.

Docs: a "P2P" section in `doc/bin/cli.md` beside the LAN cluster one.

## Required

- [Native data channel transport](/quest/m2/p2p/webrtc.md)
- [Native pairs over ICE and QUIC](/quest/m2/p2p/native-quic.md) - the native-to-native path
- [Signaling and policy](/quest/m2/p2p/signal.md) - the roster and offer schema this mirrors
