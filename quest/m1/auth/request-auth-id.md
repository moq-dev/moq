# [M] Requests name their auth on the lite wire

## Goal

In `moq-lite-07-wip`, AUTH carries an `ID (i)` the opener chooses
(increasing, never reused), and SUBSCRIBE, SUBSCRIBE_UPDATE, FETCH,
ANNOUNCE_REQUEST, ANNOUNCE_START and ANNOUNCE_UPDATE carry `Auth (i)`: 0 for
unnamed, otherwise the ID plus 1. A named request is admitted only by that auth's
grant and ends when the auth ends or narrows away from it. The presenter
names auths automatically. The `draft-lcurley-moq-auth` extension mirrors it
for moq-transport, additively.

## Plan

Step 2 of the unified auth model settled on
[#5148](https://github.com/moq-dev/moq/pull/5148) on 2026-10-15. Step 1
(#5148) makes a moq-transport request token an auth on the shared
`requests()` queue; the decisions are recorded in the
[auth README](/quest/m1/auth/README.md).

- ✅ Decided 2026-10-15 (maintainer): one kind of auth. Each request names at
  most one (`auth=id`); moq-transport's per-request token is only an encoding
  of that model, never a separate API.
- Settled in the #5148 design: at most one token per moq-transport request.
  Distinct tokens are refused NOT_SUPPORTED, and a repeated identical token is
  refused MALFORMED_AUTH_TOKEN. Test both.
- ✅ Decided 2026-10-15 (maintainer): an unnamed request is admitted by the
  union of session-lifetime auths only, so a request-lifetime token never
  widens tokenless requests.
- ✅ Decided 2026-10-15 (maintainer): a named request outside its auth's grant
  is refused, even when session scope would admit it, and it ends with its
  auth.
- ✅ Decided 2026-10-15 (maintainer): spec it in lite-07-wip now, before the
  cut, so it doesn't cost lite-08.
- A request naming an ID that hasn't arrived yet waits for it while holding
  its slot; one naming an ID that already ended is refused UNAUTHORIZED. The
  wait spends only the requester's own session budget (its stream and
  subscription limits), and ends when the AUTH arrives, the requester cancels,
  or the session closes; no timer.
- Renewal on lite: add a new auth, SUBSCRIBE_UPDATE (ANNOUNCE_UPDATE for a route) the request to it, drop
  the old one. AUTH and Subscribe streams are unordered, so dropping the old
  auth must not race the update into an UNAUTHORIZED reset: settle what tells
  the renewer the update applied before it drops the old auth, and spec it.
- moq-transport: AUTH's Request ID becomes the auth id, and a new request
  parameter names it. The alias types map as: USE_VALUE registers a
  request-lifetime auth and names it; REGISTER registers a session-lifetime
  auth and names it for the carrying request, so that request ends when the
  auth ends or narrows away from it; USE_ALIAS on a later request names it;
  DELETE withdraws one.
- An AUTH with ID `u64::MAX` is refused, since no request can name it as
  ID plus 1. Cover the boundary in the Rust and JS interop tests.
- Extend `rs/moq-net/benches/auth.rs` with a sweep over live auths and named
  requests, measuring the end or narrowing of one auth.
- Update `drafts/draft-lcurley-moq-lite.md` and
  `drafts/draft-lcurley-moq-auth.md`, Rust and `js/net`, and run
  `just test interop --all`. Update `doc/concept/moq-lite.md`'s
  Authorization section (scope is no longer only the session union) and
  `doc/concept/standard.md`, which describes only moq-transport request tokens.

Public API: none beyond step 1's `requests()` and `add()`; naming is
automatic. Wire: lite-07-wip AUTH and six request messages gain fields.
