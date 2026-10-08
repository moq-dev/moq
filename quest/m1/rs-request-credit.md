# [S] Rust requests fail fast without stream credit

## Goal

A moq-net request (subscribe, track info, fetch, announce interest; lite and
moq-transport) whose stream open finds no stream credit fails at once with a
local error, instead of waiting for the peer to grant more. A relay resetting a
downstream stream because of it sends `Internal`. Publisher group streams are
out of scope.

## Plan

Decided 2026-10-07 with [JS requests](/quest/m1/js-request-deadline.md): a
request without credit is refused, which sheds load rather than parking work
the caller can't see. Accepted: a relay asking a peer that grants few streams
gets hard failures in bursts where it used to wait, and nothing retries them.
Rust gets no answer timer: a request lives until it is answered, its demand
leaves, or the session closes, and a relay shouldn't kill a subscribe on a
slow upstream hop.

A single `Pending` poll of `open_bi` is not "no credit": on WebTransport,
`web-transport-quinn` opens the QUIC stream and then writes the session header,
which can be pending under send-window pressure. Check credit at the QUIC
level instead, likely a non-waiting open on the transport trait. Also note that
`moq-tokio`'s `poll_open_bi` parks an abandoned open in the session's slot
rather than dropping it, so the next request inherits it; the non-waiting open
should avoid that. The async backends `moq-tokio` wraps (qmux, iroh, noq) have
no credit query in `web-transport-trait`; prefer adding one upstream in
moq-dev/web-transport over leaving them waiting, and say in the PR which
backends fail fast.

Propose the error variant's name in the PR. Public API: one new `Error`
variant, and possibly a transport-trait method. Wire: none.
