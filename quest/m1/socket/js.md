# [S] JS sockets

## Goal

`@moq/net` exports `socket` with the same `Endpoint` and `Stream` surface as
Rust, and a Rust endpoint and a JS endpoint dial and accept sessions with each
other through a relay, carrying messages both ways.

## Plan

Follow `doc/concept/socket.md` and mirror the Rust surface
(`rs/moq-net/src/model/socket.rs`): an `Endpoint` announcing
`<prefix>/.tcp/<id>`, `dial(peer)` and `accept()` in JS's casing, an incoming
dial to accept or reject, and a `Stream` with `producer` and `consumer` fields
plus `peer` and `session`, and no message methods of its own. Session tracks
are `<dialer>/<session>` in both endpoints, served only on request so no
listing names them, and the reader subscribes reliably and in order. Add the
Rust/JS pair to `just test interop`, and document the export in
`doc/lib/js/net.md`.
