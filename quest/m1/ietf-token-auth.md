# [M] A moq-transport request token is one auth per distinct token

## Goal

On a moq-transport session, every request carrying the same AUTHORIZATION
TOKEN T shares one auth: T is verified once, when the first open request
names it, and its grant admits exactly the requests that name T. A request
without T never gets T's grant, and a request naming T is held to T's grant
alone. Any request type may carry a token. T's auth ends, and the relay ends
its lease at the auth server, when the last open request naming it ends; a
later request naming T verifies it again.

## Plan

Decided with the maintainer on 2026-10-28 (`/quest-plan`, "what's not worth
supporting in IETF"). The maintainer's idea: requests with the same
AUTHORIZATION TOKEN implicitly use an AUTH of that token. Keep the
feature, since [moq-dev/moq#5148](https://github.com/moq-dev/moq/pull/5148)
added it for an external user's peer, whose authority comes per request.

Decision prompts:

- Cut request tokens: ✅ keep, as an implicit AUTH per distinct token; cut.
- Reach: "widens the session" was picked, then corrected by the maintainer:
  ✅ the same token is required on every request, so a token on a SUBSCRIBE
  grants nothing to other requests.
- Grant: ✅ T's grant alone; scope plus T.
- Lifetime: ✅ until the last naming request ends, bounded by the request
  limit; until expiry or session end.
- Request types: ✅ any. This takes the incoming half of
  [Token on FETCH](/quest/m3/auth-token-fetch.md).

What changes from #5148:

- One verification and one `requests()` auth per distinct T per session,
  instead of one per request. Two requests naming the same bytes share a
  lease.
- TRACK_STATUS, FETCH, SUBSCRIBE_NAMESPACE, and the rest are admitted by a
  token they carry, not only SUBSCRIBE and PUBLISH_NAMESPACE.
  `auth::RequestKind` covers them through the request gate from
  [moq-dev/moq#5782](https://github.com/moq-dev/moq/pull/5782). A joining
  FETCH keeps following its subscription.
- Several distinct tokens on one request: check whether
  `NOT_SUPPORTED` for more than one is still needed, or whether each
  becomes its own auth. Pick the simpler one and record it in the PR.
- A REQUEST_UPDATE carrying a new token moves the request to that token's
  auth. The old auth ends if no other request names it.
- The relay tells the auth server when T's auth ends, through a call on
  `moq_auth::Client` or an `end` for the attachment. Name it while
  implementing. This folds in the deleted m3 "Relay request tokens" quest.
- Keep the rule that the tracks we subscribe under a token-authorized
  PUBLISH_NAMESPACE are admitted on that announce.

Tests on mocked time: two requests naming T verify once and share one grant.
A request without T is refused where only T covers it. T's auth ends with
its last request and verifies again on the next. Expiry and revocation end
every request naming T. A FETCH and a TRACK_STATUS are admitted by their own
token. Through the relay, the auth server sees T's lease end. Update the
token bullet of `doc/concept/standard.md` and `doc/bin/relay/auth.md`.

Public API: possibly additive on `moq_auth::Client` for the lease end. Wire:
a token on any request is honored, and repeating it on a request no longer
re-verifies.

## Related

- [Request tokens beyond Rust](/quest/m3/request-token-surfaces.md) - outgoing and non-Rust surfaces, still waiting on a consumer
- [Token on FETCH](/quest/m3/auth-token-fetch.md) - our outgoing TRACK_STATUS and FETCH carrying a token
