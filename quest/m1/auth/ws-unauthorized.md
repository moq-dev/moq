# [S] A refused WebSocket token closes the session as Unauthorized

## Goal

A relay that refuses a client's token over WebSocket reports it the same way
it does over QUIC: as a session-level `Unauthorized` close. The JS client then
stops reconnecting and sets `connection.error`, instead of retrying forever on
what looks like an outage.

## Plan

`rs/moq-relay/src/websocket.rs` answers a failed admit with HTTP 401 before
the upgrade. Browsers do not expose an upgrade status, so the client sees a
generic failure and treats it as retryable. Over QUIC the refusal is
`request.reject(Reject::Unauthorized)` (`rs/moq-relay/src/connection.rs`).

Decided (2026-10-04): on a refused admit or an empty grant, `serve_ws`
completes the upgrade and closes the session with the code `Request::reject`
uses. This deletes the HTTP-status branch and needs no JS change. Update
`doc/bin/relay/auth.md` if it names the 401.

Tests: a WebSocket connect with a bad token ends in a JS `SessionError` with
the unauthorized code, and the reload loop stops.

## Closes

- [#4786](https://github.com/moq-dev/moq/issues/4786) - close this issue when the quest finishes

## Related

- [Expired token error](/quest/m1/auth/expired-error.md) - builds on a session-level refusal on both transports
