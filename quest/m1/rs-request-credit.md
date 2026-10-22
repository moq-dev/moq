# [S] Rust requests fail fast without stream credit

## Goal

A moq-net request (subscribe, track info, fetch, announce interest; lite and
moq-transport) whose stream open finds no stream credit fails at once with a
local error, instead of waiting for the peer to grant more. This holds on
every in-tree backend; iroh, an accepted exception that stays on upstream
noq, keeps waiting. A relay resetting a
downstream stream because of it sends `Internal`. Publisher group streams are
out of scope.

## Plan

Decided 2026-10-07: a request without credit is refused, which sheds load
rather than parking work the caller can't see. Accepted: a relay asking a peer that grants few streams
gets hard failures in bursts where it used to wait, and nothing retries them.
Rust gets no answer timer: a request lives until it is answered, its demand
leaves, or the session closes, and a relay shouldn't kill a subscribe on a
slow upstream hop.

A single `Pending` poll of `open_bi` is not "no credit": on WebTransport,
moq-tokio's `quic::web` opens the QUIC stream and then writes the session header,
which can be pending under send-window pressure. Check credit at the QUIC
level instead, likely a non-waiting open on moq-net's `transport::poll`
session trait. Also note that
`moq-tokio`'s `poll_open_bi` parks an abandoned open in the session's slot
rather than dropping it, so the next request inherits it; the non-waiting open
should avoid that. `quic::web` is in-tree and can ask `moq-quic` directly,
and the in-tree qmux (`rs/qmux`) drives `moq-quic`'s stream state too, so
its session can ask the same way. iroh, wrapped through
`web-transport-trait`, has no credit query and, as an accepted exception,
keeps waiting. This quest gives the qmux session the non-waiting open. Say in the PR which backends fail fast.

Propose the error variant's name in the PR. Public API: one new `Error`
variant, and possibly a `transport::poll` session method. Wire: none.

Decided 2026-10-20 (maintainer): the `transport::poll` trait breaks land in
order, this quest on the receive-lease shape from
[moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878), then the send
domain in [broadcast fairness](/quest/m2/broadcast-fairness.md), so
implementers rebase on one shape at a time.
