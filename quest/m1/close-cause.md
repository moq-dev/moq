# [S] Every session close records its cause

## Goal

A session closed for a protocol violation, or abandoned at shutdown before
its SETUP reply, records why, over every transport, instead of a clean end
or nothing.

## Plan

The session records its close cause (`Closing` in
`rs/moq-net/src/session.rs`), but some closes bypass it and close the
transport directly:
`lite::error::control` (`rs/moq-net/src/lite/error.rs`),
`request_update::too_many` (`rs/moq-net/src/ietf/request_update.rs`), and the
IETF dispatcher's protocol-violation close. Over QUIC those still record a
clean end. Route them through the close cause.

The relay's shutdown drain drops `request.ok()` at its deadline
(`until_deadline` in `rs/moq-relay/src/connection.rs`, the `select` in
`rs/moq-relay/src/uring.rs`), so an abandoned SETUP reply records nothing.
Record it as the handshake deadline already does.

Public API: none. Wire: none.
