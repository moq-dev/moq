# [S] JS sockets

## Goal

`@moq/net` exports `socket` with the same `Endpoint`, `TcpStream`, and
`TrackStream` surface as Rust, and a Rust endpoint and a JS endpoint dial and
accept sessions in both protocols with each other through a relay, carrying
data both ways.

## Plan

Follow `doc/concept/socket.md` and mirror the Rust surface
(`rs/moq-net/src/model/socket.rs`): an `Endpoint` announcing
`<prefix>/.socket/<id>`; `dialTcp(peer)` / `acceptTcp()` and
`dialTrack(peer)` / `acceptTrack()` in JS's casing, each landing in its
protocol's queue; an incoming dial to accept or reject; a `TcpStream` with a
group `producer` and `consumer` (one group each way: frames are writes, finish
is FIN, abort is RST, and a second group resets the session); and a
`TrackStream` with a track `producer` and a reliably subscribed `consumer`.
Both carry `peer` and `session`, and no message methods of their own. Session
tracks are `<protocol>/<dialer>/<session>` in both endpoints, served only on
request so no listing names them; an unknown or unaccepted protocol is reset
as unsupported. JS reads without fastest-reader pacing until
[JS fastest-reader pacing](/quest/m2/js-fastest-reader.md), so a JS `tcp`
reader that falls a group tail behind fails with an eviction error; say so in
the docs. Add the Rust/JS pairs to `just test interop`, and document the
export in `doc/lib/js/net.md`.
