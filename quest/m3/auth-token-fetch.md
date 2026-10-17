# [M] A request token authorizes TRACK_STATUS and FETCH

## Goal

A moq-transport peer whose only authority is a request token can ask for
TRACK_STATUS and standalone FETCH, as it already can for SUBSCRIBE and
PUBLISH_NAMESPACE. In the other direction, our own TRACK_STATUS, standalone
FETCH, and a relay's cache-miss group FETCH carry the connection's request
token when one is set. Each token is verified for its request only and ends
with it, under the same rules #5148 set for SUBSCRIBE.

## Plan

A #5148 follow-up. Today a token authorizes only SUBSCRIBE and
PUBLISH_NAMESPACE. Fetch-only demand (TRACK_STATUS, then FETCH) and
cache-miss group FETCHes still need the session scope. So a token-only
peer can subscribe to a track but cannot look it up or fetch its history.

Open question for the maintainer. This reverses a settled decision:
"standalone FETCH gains no request-token authority" (2026-10-12, in
[#5148](https://github.com/moq-dev/moq/pull/5148)'s quest and
[the request gate refactor](https://github.com/moq-dev/moq/pull/5782)).

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

Keep #5148's boundaries (✅ Decided 2026-10-15, maintainer): a token on a
request is one kind of auth on `requests()`, the request is held to its
grant alone and ends with it, every token is verified, at most one per
request, and a peer's AUTH_OK is a ceiling, never authority. Outgoing
tokens wait for `add()` with automatic naming, so the outgoing half here
lands with or after that. A joining FETCH keeps following its subscription's
authorization and carries no token of its own.

Tests: incoming TRACK_STATUS and FETCH admitted by a verified token outside
the session grant, refused when the verifier refuses, and ended when the
grant ends while the response waits on credit. Outgoing requests carry the
token. Run cross-language interop for the wire change.

Public API: none expected beyond widening what `auth::RequestKind` names.
Wire: the existing AUTHORIZATION TOKEN parameter (0x03), now sent and honored
on TRACK_STATUS and FETCH.
