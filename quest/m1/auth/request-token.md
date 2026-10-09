# [L] A token on a request authorizes that request

## Goal

A moq-transport peer authorizes a SUBSCRIBE or PUBLISH_NAMESPACE with the
`AUTHORIZATION TOKEN` parameter (`0x03`) on that request, and refreshes it in
band with a REQUEST_UPDATE, on every supported draft. A request is authorized
by the session's grant first; when that does not cover it, by the token on the
request; with neither it is refused `UNAUTHORIZED`. The token's grant covers
only the request it rode on, never joins the session union, and ends with that
request. On any other request the token is ignored.

## Plan

[#4675](https://github.com/moq-dev/moq/pull/4675) builds this and is in manual
maintainer review. Decided 2026-10-09: an external moq-transport deployment
needs per-request tokens with in-band renewal, so this quest keeps its full
scope, reversing the 2026-10-08 shrink. The pieces that stand alone land first
as the quests under Required; #4675 then rebases onto them and onto the line,
which already routes draft-14/15/16 updates to their target (#4961), and drops
its own copies.

Decided, so review does not relitigate them:

- **Admission.** A request the session grant covers is served without
  verifying its token. Otherwise the token becomes an `auth::Request` on the
  session's `auth::Handle`, marked with the request's path and kind, and the
  acceptor's `Grant` is checked against that request alone. With no
  `requests()` consumer a non-empty token is refused `Unsupported`.
- **Scope.** Honored on SUBSCRIBE, PUBLISH_NAMESPACE, and a REQUEST_UPDATE
  renewing one; ignored on every other request (2026-10-05).
- **Refused renewal** follows drafts 16 section 9.11.1 and 18 section 10.9.1:
  REQUEST_ERROR ends only that request, with PUBLISH_DONE `UPDATE_FAILED` for
  a subscription or a closed stream for a namespace. The session stays up and
  the old grant does not survive. A lapse or acceptor revoke also ends only
  that request (2026-10-05).
- **Client credential.** `auth::Handle::set_request_token`, beside session
  tokens, with no `Client` methods. moq-tokio's `Connection` owns the token
  across reconnects: `Connection::auth()` renews it on a live connection and
  `connect::Config::with_request_token` seeds it. Setting a new token
  re-presents it on live requests. This takes the request-token slice of
  `Connection::auth()` from [Relay tokens](/quest/m1/auth/relay-refresh.md)
  (2026-10-01 Q1 and Q3, 2026-10-05).
- **Update credit.** Draft-19+ advertises and enforces MAX_REQUEST_UPDATES,
  closing with TOO_MANY_REQUEST_UPDATES (0x1B); earlier drafts keep a local
  guard that ends only the request. The sender keeps one renewal in flight per
  subscription.
- **Drafts.** Renewal works on every supported draft, 14 through 16 included
  (2026-10-09).
- `EXPIRED_AUTH_TOKEN` and `MALFORMED_AUTH_TOKEN` land with
  [Expired token error](/quest/m1/auth/expired-error.md) (2026-10-01 Q4).

Fix in #4675 before it merges, each with a regression test. Found by a
2026-10-09 read of head `29ed74e78`, not yet reproduced:

- The publisher answers a token-less REQUEST_UPDATE while a renewal's verdict
  is still pending (`handle_renewal_update`), so on draft-17+, where answers
  are unkeyed, they go out of order.
- The new 0x03 fields decode as `Option`, refusing a repeat the drafts allow;
  [Request-token decode](/quest/m1/auth/request-token-decode.md) settles the
  rule.
- The admission sequence appears four times and the receiver renewal state
  machine twice (publisher and subscriber); each becomes one helper.
- `set_request_token` takes `setup::Token`, not encoded bytes, so a caller
  cannot send an alias form. `auth::Request`'s `path`, `kind`, and
  `token_kind` become one `Option`.
- Setting a request token on a moq-lite session fails loud instead of doing
  nothing.
- Drop the outbound update sniffer in `ietf/adapter.rs`, which re-parses an ID
  the sender already holds, and the unread `Peer::max_request_updates` and
  `token::decode_value`.

Open for review: one `requests()` consumer receives both session and request
tokens, so an acceptor written for session tokens also answers request tokens.

Public API: additive. moq-net gains `auth::RequestKind`, the request a
`auth::Request` belongs to, `auth::Handle::set_request_token`, and
`SessionError::TooManyRequestUpdates`. moq-tokio gains `Auth`,
`Connection::auth`, `connect::Config::with_request_token`, and
`server::Request::auth`. Wire: MAX_REQUEST_UPDATES (0x08) on draft-19+ and
TOO_MANY_REQUEST_UPDATES (0x1B); the parameter already exists in every
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
