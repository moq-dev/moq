# [S] Relay HTTP surface

## Goal

The relay's HTTP `/fetch` response cannot be sniffed as active content on the
relay origin or stored by a shared cache, and no web page can reach the
internal ops listener: it refuses a request carrying an `Origin` header,
which blocks a cross-site `POST /sessions/revalidate` even to its literal
address, and a request whose `Host` is not its own, which blocks
DNS-rebinding reads of `GET /sessions`.

## Plan

From the 2026-10-27 audit (RELAY-6, RELAY-7). `/fetch` in
`rs/moq-relay/src/web.rs` streams publisher bytes with no `Content-Type`;
`/announced` returns a `String`, so axum already sends `text/plain`. Send
`application/octet-stream`, `X-Content-Type-Options: nosniff`, and
`Cache-Control: private, no-store` on authenticated responses.

`rs/moq-relay/src/internal.rs` is documented "keep private" and has no auth.
A `Host` check alone does not stop a form posting to `http://127.0.0.1:port`,
whose `Host` matches; browsers send `Origin` on such a POST, while ops tools
such as `curl` send none. A bearer token was not chosen because the listener
is meant to stay private. Tests cover a direct-address POST with `Origin`
and a rebinding GET with a foreign `Host`. Update `doc/bin/relay/` if the listener's contract
changes.

Public API: none. Wire: none.
