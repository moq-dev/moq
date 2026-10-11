# [L] A token on a request authorizes that request

## Goal

A moq-transport peer authorizes a SUBSCRIBE or PUBLISH_NAMESPACE with the
`AUTHORIZATION TOKEN` parameter (`0x03`) on that request, and refreshes it in
band with the draft's update: SUBSCRIBE_UPDATE for a subscription on drafts 14
and 15, which cannot update a namespace, and REQUEST_UPDATE for either from
16. A request is authorized
by the session's grant first; when that does not cover it, by the token on the
request; with neither it is refused `UNAUTHORIZED`. The token's grant covers
only the request it rode on, never joins the session union, and ends with that
request. On any other request the token is ignored.

## Plan

[#5148](https://github.com/moq-dev/moq/pull/5148) builds this, carrying
Kyle Sletmoe's [#4675](https://github.com/moq-dev/moq/pull/4675) forward on a
branch maintainers can push to. Decided 2026-10-09: an external moq-transport
deployment needs per-request tokens with in-band renewal, so this quest keeps
its full scope, reversing the 2026-10-08 shrink. The pieces that stand alone
land first as the quests under Required; #5148 stacks on both.

Decided, so review does not relitigate them:

- **Admission.** A request the session grant covers is served without
  verifying its token. Otherwise the token becomes an `auth::Request` on the
  session's `auth::Handle`, marked with the request's path and kind, and the
  acceptor's `Grant` is checked against that request alone. With no
  `requests()` consumer a non-empty token is refused `Unsupported`.
- **Scope.** Honored on SUBSCRIBE, PUBLISH_NAMESPACE, and an update renewing
  one, SUBSCRIBE_UPDATE included; ignored on every other request (2026-10-05).
- **Refused renewal** follows drafts 16 section 9.11.1 and 18 section 10.9.1:
  REQUEST_ERROR ends only that request, with PUBLISH_DONE `UPDATE_FAILED` for
  a subscription. A namespace ends with a closed stream from 17, and with
  PUBLISH_NAMESPACE_CANCEL on 16, where it shares the control stream (16
  section 9.24). The session stays up and the old grant does not survive. A
  lapse or acceptor revoke also ends only that request (2026-10-05).
- **Client credential.** `auth::Handle::set_request_token`, beside session
  tokens, with no `Client` methods. moq-tokio's `Connection` owns the token
  across reconnects: `Connection::auth()` renews it on a live connection and
  `connect::Config::with_request_token` seeds it. Setting a new token
  re-presents it on live requests. This owns the request-token slice of
  `Connection::auth()`, so it does not wait on
  [Relay tokens](/quest/m1/auth/relay-refresh.md): whichever lands first adds
  the handle (2026-10-01 Q1 and Q3, 2026-10-05).
- **Update credit.** Draft-19+ advertises and enforces MAX_REQUEST_UPDATES,
  closing with TOO_MANY_REQUEST_UPDATES (0x1B); earlier drafts keep a local
  guard that ends only the request. 0x1B joins the shared session registry as
  [Request-token decode](/quest/m1/auth/request-token-decode.md) does for 0x13
  and 0x17: `SessionCode` in `js/net`, moq-lite's Session Error Codes table,
  and `session_codes_round_trip`. The sender keeps one renewal in flight per
  subscription on drafts that answer an update. Draft-14 never answers an
  accepted SUBSCRIBE_UPDATE, so its sender does not wait for one; test two
  successive replacements with no answers.
- **Drafts.** Renewal works on every supported draft, 14 through 16 included
  (2026-10-09), tested with SUBSCRIBE_UPDATE on 14 and 15. A namespace renews
  from 16, whose REQUEST_UPDATE covers PUBLISH_NAMESPACE.
- `EXPIRED_AUTH_TOKEN` and `MALFORMED_AUTH_TOKEN` are not this quest's; they
  land with [Expired token error](/quest/m1/auth/expired-error.md), which
  does not block it (2026-10-01 Q4).

Of what a 2026-10-09 read of #4675 found, #5148 fixes updates answered out of
order behind a pending renewal, a repeated 0x03 refused, duplicated admission
and renewal logic, three parallel `Option`s on `auth::Request`, a request token
silently ignored on moq-lite, an outbound update sniffer, unread
`max_request_updates` and `decode_value`, and a refused renewal overloading
`Error::Unsupported`. Still open in #5148, to fix before it merges:

- A namespace renews only from draft 17; a token-bearing draft-16
  REQUEST_UPDATE for a PUBLISH_NAMESPACE is still ignored. Implement it
  (decided 2026-10-09: draft-16 allows it, and the Drafts decision covers 16),
  with a test that a refused one sends PUBLISH_NAMESPACE_CANCEL.

Decided 2026-10-09: `set_request_token` keeps the typed `setup::Token` and
the encoder writes only `USE_VALUE`. Alias forms wait for a consumer that
needs them, rather than an untyped bytes API now.

Open for review: one `requests()` consumer receives both session and request
tokens, so an acceptor written for session tokens also answers request tokens.

Public API: additive. moq-net gains `auth::RequestKind`, `auth::Scope`,
`auth::Request::scope`, `auth::Handle::set_request_token`, and
`SessionError::TooManyRequestUpdates`, mirrored as a `js/net` `SessionCode`.
moq-tokio gains `Auth`,
`Connection::auth`, `connect::Config::with_request_token`, and
`server::Request::auth`. Wire: MAX_REQUEST_UPDATES (0x08) on draft-19+ and
TOO_MANY_REQUEST_UPDATES (0x1B), which also joins moq-lite's registry; the
parameter already exists in every
supported draft.

Follow-ups, planned when a consumer needs them: a JS request-token setter and
accept-side `requests()`, a moq-cli `--request-token`, and the setter through
moq-ffi in [Bindings](/quest/m1/auth/bindings.md).

## Required

- [Request-token decode](/quest/m1/auth/request-token-decode.md) - a request
  token decodes by the draft's rules and its forbidden forms close the session
- [Setup extensions](/quest/m1/auth/extensions.md) - a side declares which
  Setup extensions it offers with one `Extensions` struct

## Related

- [Request leases](/quest/m1/auth/request-lease.md) - moq-relay honors a
  request token with a lease of its own
