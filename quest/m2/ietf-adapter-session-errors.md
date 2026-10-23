# [S] The draft 14-16 adapter keeps session errors

## Goal

On moq-transport drafts 14 to 16, a session-level transport error from
`ControlStreamAdapter` reaches the session with its cause instead of as
`Closed`. A peer that ends the session with a terminal error, such as
ProtocolViolation or Unauthorized, is not redialed as if the drop were
transient.

## Plan

Decided 2026-10-22 (maintainer, from the release audit): m2. The race is
narrow and limited to the older drafts. It is the session-scope follow-up
left by [moq-dev/moq#6251](https://github.com/moq-dev/moq/pull/6251), which
kept stream errors through `AdapterError::Real`.

In `rs/moq-net/src/ietf/adapter.rs`, the adapter's `Session` impl maps the
native `poll_accept_bi`, `poll_open_uni`, `poll_accept_uni`, the datagram
send and receive, and `poll_closed` errors to `Error::Closed`. In
`ietf/session.rs`, `adapter_run` reads the real SETUP stream first and usually
keeps the peer's code. The code is lost when a unis, dispatch, or datagram
future fails first. The session then reports `Closed`, and
`moq-tokio`'s `is_terminal` treats that as retryable.

Carry the transport error through the way `AdapterError::Real` does, mapped
with the negotiated registry. Reproduce first: a draft-14 peer closes with
ProtocolViolation while a uni or datagram poll is pending, and the client
must see a terminal error and not redial.

Public API: none. Wire: none.
