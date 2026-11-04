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

Incoming requests carry claimed names; accepting checks that the original dial
still answers before publishing a new half. Controlled late-query cases for both
protocols verify that a closed session cannot be accepted again or receive the
fresh session's content. The relay refusal probe runs before data sessions, so it
uses one rejection independently of late requests for completed halves. Each
subsequent incoming is verified once; canceled claims are declined before the
next valid dial, as required by the unverified Incoming contract.

Socket request budgets now use the established `.socket/<id>` namespace to wait
until caller cancellation or session closure. Controlled one-hour virtual waits
cover both protocols on lite-07 and IETF18/20/22. Ordinary request budgets are
unchanged. The independent JS IETF subscriber deadline constructors overlap one
file owned by the shared adapter PR; its absolute-start correction stays there.

## Waiting

- Maintainer: choose the JS request-setup budget policy exposed by socket acceptance. Local Claude review found that selecting unlimited setup by `.socket/<id>` couples the adapters to endpoint naming and leaves ordinary on-request tracks subject to the same fixed answer budget. Recommendation: replace fixed application-answer budgets with demand, cancellation, fail-fast stream credit and session closure for all on-request tracks, matching Rust, and remove the namespace exception. Alternatively retain transport-only setup budgets with an explicit internal phase distinction that excludes application-held answers. This changes ordinary request behavior beyond the settled socket port, so it needs a maintainer choice; fix and validate the selected policy in this PR, without a separate follow-up quest. No broader budget investigation or production change was made during closeout.
- Maintainer: choose the generic adapter default in [moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883). Its owner recommends canonical unrestricted/from-zero semantics, explicitly accepting retained-history replay for third-party filterless/no-fill subscriptions. Alternatively preserve filterless live joins, distinguish omitted filters from explicit absolute zero in both adapters, and settle draft20's encoding/presence rule. This JS socket port leaves that choice open; explicit socket absolute group0/frame0 remains settled.
- moq-dev/moq: merge the generic IETF absolute-start correction from [moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883), then verify all eight early-answer cases and the relay matrix pass on the JS PR's actual head. Absolute group 0 alone does not preserve early bursts on the current adapters; no new socket API or wire fields are needed.
