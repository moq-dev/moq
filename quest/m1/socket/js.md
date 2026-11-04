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
tracks are `<protocol>/<dialer>/<acceptor>/<session>` in both endpoints, served only on
request so no listing names them; an unknown or unaccepted protocol is reset
as unsupported. JS reads without fastest-reader pacing until
[JS fastest-reader pacing](/quest/m2/js-fastest-reader.md), so a JS `tcp`
reader that falls a group tail behind fails with an eviction error; say so in
the docs. Add the Rust/JS pairs to `just test interop`, and document the
export in `doc/lib/js/net.md`.

The JS implementation, documentation, relay lanes, and endpoint/session benchmark
are present. Every new session subscribes from absolute group 0. The controlled
metadata-before-burst regressions stay enabled: lite-07 passes, while draft 18
stalls and drafts 20/22 lose the earlier track groups on the uncorrected base.
The generic IETF adapter correction belongs to
[moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883), together with the
Rust socket's matching start correction, and stays out of this JS socket diff.
After it lands, merge main and rerun the controlled cases, `just ci`,
`just test interop --all`, and local cross-vendor review on the resulting head;
then remove this quest and mark the JS PR ready.

The merged-source all-language run passes every non-socket lane and 27 of 32
socket lanes. Two draft18 JS-dialer TCP lanes fail during initial data. Three
Rust-dialer TCP lanes (Node draft18/20 and Bun draft18) pass data, FIN, and RST,
then stall after JS rejects an incoming request. Check whether late requests for
a reset session are resurfacing before assuming that incoming request is the
fresh rejected dial; preserve the refusal assertion. The temporary dependency
run passed all eight controlled bursts, but still had the draft20 refusal stall.
It does not establish final-head interoperability.

## Waiting

- Maintainer: choose the generic adapter default in [moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883). Its owner recommends canonical unrestricted/from-zero semantics, explicitly accepting retained-history replay for third-party filterless/no-fill subscriptions. Alternatively preserve filterless live joins, distinguish omitted filters from explicit absolute zero in both adapters, and settle draft20's encoding/presence rule. This JS socket port leaves that choice open; explicit socket absolute group0/frame0 remains settled.
- moq-dev/moq: merge the generic IETF absolute-start correction from [moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883), then verify all eight early-answer cases and the relay matrix pass on the JS PR's actual head. Absolute group 0 alone does not preserve early bursts on the current adapters; no new socket API or wire fields are needed.
