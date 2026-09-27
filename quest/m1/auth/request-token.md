# [M] A token on a request authorizes that request

## Goal

A moq-transport peer that is not on moq-dev can present or refresh a
credential the draft-17+ way, with the `AUTHORIZATION TOKEN` parameter
(`0x03`) on SUBSCRIBE, REQUEST_UPDATE, PUBLISH, FETCH, PUBLISH_NAMESPACE,
SUBSCRIBE_NAMESPACE, TRACK_STATUS, or any other request that carries
parameters. A request is authorized by the session's grant first; when that
does not cover it, by the token on the request; with neither it is refused
`UNAUTHORIZED`. The token's grant covers only the request it rode on and
lives exactly as long as that request, and a REQUEST_UPDATE carrying a new
token replaces it, which is how a peer refreshes. It scopes by path, never
by method. Today the strict decoder on newer drafts fails the whole message
on the unknown key, and the legacy drafts silently ignore it.

## Plan

- Decode with the SETUP option's structure and rules
  (`rs/moq-net/src/ietf/token.rs`, `js/net/src/ietf/token.ts`): `USE_VALUE` yields the token, `REGISTER` is a value since we
  advertise no `MAX_AUTH_TOKEN_CACHE_SIZE`, and `DELETE` or `USE_ALIAS`
  closes with `PROTOCOL_VIOLATION`. Both decoder families change: the strict
  `decode_params!` path, which rejects the key today, and the generic KVP
  path the legacy drafts use, which ignores it.
- Fallback only: a request the session grant already covers is served
  without verifying its token. Otherwise its token becomes an
  `auth::Request` on the session's `auth::Handle`, the seam an AUTH stream's
  token takes, marked as belonging to that one request (its path and kind).
  The `Grant` the acceptor answers is checked against that request alone,
  never joins the session union, and ends when the request ends. With no
  `requests()` consumer the default acceptor rejects a non-empty token
  `Unsupported`, so an app that does not opt in refuses every request that
  needs one.
- The request waits on its token's verdict before it is resolved. A refused
  or insufficient token refuses the request with the verdict's code
  (`UNAUTHORIZED`, `EXPIRED_AUTH_TOKEN`, `MALFORMED_AUTH_TOKEN`, or
  `NOT_SUPPORTED`) through `to_code` for the draft; the session continues.
- Refresh: a REQUEST_UPDATE with a token verifies it the same way and, once
  accepted, replaces the request's grant; a refused one leaves the old grant
  until it lapses. When a request's grant expires or is revoked, that request
  alone ends with `EXPIRED_AUTH_TOKEN` or `UNAUTHORIZED`. A session grant
  that shrinks cancels the requests it covered, as for any request, through
  [Origin narrowing](/quest/m1/auth/narrowing.md).
- Relay: each such request attaches its own lease through the
  `Client::attach` path [Relay tokens](/quest/m1/auth/relay-refresh.md)
  builds, once per request, with no sharing across requests carrying the
  same bytes; the lease is dropped with the request. The request is resolved
  against the origin with the path checked against that lease's grant, not
  through the session's scoped origin handle.
- `js/net` mirrors the decode and the default refusal.
- Docs: `doc/bin/relay/auth.md` states the order (session grant, then the
  request's token, then `UNAUTHORIZED`), that a request token covers only its
  request, and how a peer refreshes with REQUEST_UPDATE.
- Tests: a SUBSCRIBE outside the session grant succeeds with a covering token
  and is refused `UNAUTHORIZED` without one; its token grants nothing to a
  second SUBSCRIBE; a request inside the session grant never calls the
  verifier; a REQUEST_UPDATE token keeps a subscription alive past the old
  token's expiry; an expired request token ends only that request; with no
  consumer a token-bearing request is refused `Unsupported`; one legacy and
  one strict draft, Rust and JS.

Public API: additive on `moq_net::auth::Request` (the request it belongs
to). Wire: none new; the parameter already exists in every supported draft.

## Required

- [Relay tokens](/quest/m1/auth/relay-refresh.md) - supplies the per-token
  lease and `Client::attach` path each request uses
