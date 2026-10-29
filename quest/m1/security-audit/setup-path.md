# [S] SETUP Path refused on URL bindings

## Goal

A client on a binding whose handshake carries a request URI (WebSocket,
WebTransport) that also sends a SETUP Path parameter is closed with
PROTOCOL_VIOLATION, as `drafts/draft-lcurley-moq-lite.md` already requires.
moq-tokio and moq-uring authorize and route the same path for the same
connection.

## Plan

From the 2026-10-27 audit (RELAY-1). Today `Request::path` and
`Request::query` in `rs/moq-tokio/src/server.rs` prefer a non-empty SETUP
Path over the URL, while moq-uring's listener uses the URL path. A proxy or
CDN that routes or authorizes by URL path is bypassed on moq-tokio, and the
two runtimes disagree on which path a session gets.

Decided 2026-10-27 (maintainer): refuse, rather than silently ignore the
SETUP Path. Enforce it once in the runtime-agnostic handshake code so both
runtimes share it, then simplify `path()`/`query()` to read the URL on those
bindings. A test covers both runtimes.

Public API: none expected. Wire: enforcement of an existing draft MUST; no
draft change.
