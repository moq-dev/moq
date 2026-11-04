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
the docs. As in Rust, dials and accepts fail as unsupported while the
endpoint's connection is moq-lite before 07, which never carries its hidden
broadcast, instead of waiting forever; test it against a lite-06 connection.
Add the Rust/JS pairs to `just test interop`, and document the export in
`doc/lib/js/net.md`.

The JS implementation, documentation, relay lanes, endpoint/session benchmark,
the lite-06 unsupported check, and the lifecycle-bounded track requests below
are present. Every new session subscribes from absolute group 0. The controlled
metadata-before-burst regressions stay enabled: lite-07 passes, while draft 18
stalls and drafts 20/22 lose the earlier track groups on the uncorrected base.
The generic IETF adapter correction belongs to
[moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883), together with the
Rust socket's matching start correction, and stays out of this JS socket diff.
After it lands, merge main and rerun the controlled cases, `just ci`,
`just test interop --all`, and local cross-vendor review on the resulting head;
then remove this quest and mark the JS PR ready.

Incoming requests carry claimed names; accepting checks that the original dial
still answers before publishing a new half. Controlled late-query cases for both
protocols verify that a closed session cannot be accepted again or receive the
fresh session's content. The relay refusal probe runs before data sessions, so it
uses one rejection independently of late requests for completed halves. Each
subsequent incoming is verified once; canceled claims are declined before the
next valid dial, as required by the unverified Incoming contract.

Decisions:

- ✅ 2026-11-04 (maintainer): JS request-setup budgets use general lifecycle
  bounds, like Rust. Every on-request track request (subscribe, track info,
  fetch, fill) waits for its answer until demand ends, the caller cancels, stream
  credit fails fast, or the session closes; no fixed application-answer budget
  and no `.socket/<id>` namespace exemption. Not chosen: transport-only phase
  budgets. Announcement discovery and namespace advertisements are not track
  requests and keep their ten-second bound; `track.Consumer.info()` takes an
  optional `signal`, its counterpart of dropping Rust's pending `info()`.
- ✅ 2026-11-04 (maintainer): [moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883)'s
  generic adapter default preserves filterless live joins. Not chosen:
  canonical from-zero. #7883 is reworked separately to do that; socket
  absolute group 0/frame 0 stays explicit.

## Waiting

- moq-dev/moq: land [moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883)
  (generic IETF absolute-start correction and Rust socket start), then merge main
  here and verify all eight early-answer cases and the relay matrix pass on the
  JS PR's actual head. Check with `gh pr view 7883 --json mergedAt`.
