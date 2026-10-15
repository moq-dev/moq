# [M] A request token authorizes TRACK_STATUS and FETCH

## Goal

A moq-transport peer whose only authority is a request token can ask for
TRACK_STATUS and standalone FETCH, as it already can for SUBSCRIBE and
PUBLISH_NAMESPACE. In the other direction, our own TRACK_STATUS, standalone
FETCH, and a relay's cache-miss group FETCH carry the connection's request
token when one is set. Each token is verified for its request only and ends
with it, under the same rules #5148 set for SUBSCRIBE.

## Plan

A #5148 follow-up. Today the token rides only SUBSCRIBE and
PUBLISH_NAMESPACE. Fetch-only demand (TRACK_STATUS, then FETCH) and
cache-miss group FETCHes still need the connection grant. So a token-only
peer can subscribe to a track but cannot look it up or fetch its history.
#5148's `a_request_token_never_authorizes_a_track_status` pins that
behavior, and this quest reverses it.

Open question for the maintainer. This reverses a settled decision:
"standalone FETCH gains no request-token authority" (2026-10-12, in
[#5148](https://github.com/moq-dev/moq/pull/5148)'s quest and
[Request gate](/quest/m1/auth/request-gate.md)).

1. (recommended) Do it in m1, after the request gate. Move this file under
   [In-band auth](/quest/m1/auth/README.md) and list it in that epic's
   Required. The gate's dispatcher then makes this one admission path for
   two more request types, not new per-handler checks. Without it, the hook's
   interoperability claim stops at SUBSCRIBE, while moq-transport peers
   routinely look a track up and fetch its history.
2. Leave it parked here with the other request-token surfaces until a
   consumer asks.

It is parked in m3 until the maintainer answers, so an undecided scope does
not gate the breaking release (from this plan's review).

Keep the hook's boundaries. Every token is verified, at most one per
request, and a peer's AUTH_OK is a ceiling, never authority. Request tokens
keep their own bound. The relay still refuses a token-carrying request
`NOT_SUPPORTED` until [Relay request tokens](/quest/m3/auth-request-lease.md).
A joining FETCH keeps following its subscription's authorization and carries
no token of its own.

Tests: incoming TRACK_STATUS and FETCH admitted by a verified token outside
the session grant, refused when the verifier refuses, and ended when the
grant ends while the response waits on credit. Outgoing requests carry the
token. Run cross-language interop for the wire change.

Public API: none expected beyond widening what `auth::RequestKind` names.
Wire: the existing AUTHORIZATION TOKEN parameter (0x03), now sent and honored
on TRACK_STATUS and FETCH.

## Required

- [Request gate](/quest/m1/auth/request-gate.md) - the dispatcher-owned gate this extends to two more request types
