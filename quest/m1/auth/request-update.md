# [L] Live requests follow their auth

## Goal

A publisher renews the credential behind a live announcement without
reopening it. A `Route` holds an optional `moq_net::auth::Auth`; when the
application swaps it, the session sends REQUEST_UPDATE (moq-transport)
carrying the new auth, and the request stays up. Lite sends nothing: a
route's auth already sits in the session union through its AUTH stream, so
lite renews through the union as in
[A request right behind its token](/quest/m1/auth/request-race.md). The first consumer is a client that keeps a
PUBLISH_NAMESPACE alive past its credential's expiry by sending
REQUEST_UPDATE with a fresh token before it expires.

## Plan

Step 3 of the unified auth model settled on
[#5148](https://github.com/moq-dev/moq/pull/5148) on 2026-10-15, ranked right
after step 1 (#5148, incoming request tokens as auths on `requests()`)
because it has a consumer now. The decisions are recorded in the
[auth README](/quest/m1/auth/README.md).

- ✅ Decided 2026-10-15 (maintainer): general request updates, not a
  per-request token setter. Each `Route`/`Subscription` carries an
  `Option<moq_net::Auth>`, and changing it triggers the update automatically.
  Unset means the session's scope.
- ✅ Decided 2026-10-15 (maintainer): outgoing tokens go through `add()`;
  #5148's `set_request_token` and its renewal sender are not carried forward.
- ✅ Decided 2026-10-16 (maintainer, #5695): `auth::Handle::add()` returns
  `auth::Auth`, a cloneable reference to one presented credential that stays
  presented until its last reference drops; `Route::auth` and
  `Subscription::auth` hold `Option<Auth>`. Not a cloneable `auth::Token`,
  which conflates the credential bytes with the auth a request names, and
  not credential bytes on the request, which bypass `add()`.
- ✅ Decided 2026-10-16 (maintainer, #5695): an `Auth` is bound to the auth
  handle that issued it. A `Route` lives on an origin that outlives any one
  session, so a connection-level handle spans that connection's sessions:
  `moq_tokio::Connection::auth().add()` returns a `moq_net::auth::Auth` every
  later session of that connection presents. Any other session sends the
  request with no auth, under its scope. Not presented by any session, which
  would send the credential to every peer the origin feeds, and not bound to
  one session, which would make the app re-add and re-announce after every
  reconnect. moq-tokio's `connection::Auth` and `connection::Token` are
  renamed so the names match across layers.
- ✅ Revisited 2026-10-16 (maintainer, #5695): the connection-level handle is
  a session-independent issuer in moq-net, which needs its own refactor, so
  it moves to [Auth outlives its session](/quest/m1/auth/issuer.md). Until
  then an `Auth` is bound to one session's handle, and a route on a
  reconnecting connection is re-added and re-announced after a reconnect.
- ✅ Decided 2026-10-16 (maintainer, #5695): the JS names mirror Rust's in
  this PR: JS `Auth` becomes `Handle` and `Token` becomes `Auth`.
- ✅ Decided 2026-10-16 (maintainer, #5695): an acceptor that accepts a
  renewal and drops the grant it replaces in one step keeps the request. The
  receiver hands over to the accepted renewal rather than ending between the
  two.
- ✅ Decided 2026-10-16 (maintainer, #5695): `Route::auth` only. A
  `Subscription` is aggregated across subscribers before it reaches the
  wire, so `Subscription::auth` and its aggregation rule are a follow-up.
- Renewing many live requests at once must not take one round trip per
  request: each request's update is its own in-flight step, polled beside the
  session's other announce and subscribe work, so new announcements and
  withdrawals never wait behind a renewal. At most one update is in flight
  per request, superseded by a newer auth while it waits for credit. Request
  IDs reach the wire in allocation order (drafts 14-16 require the next
  sequential ID), and request-ID credit bounds how many start at once, with
  no new cap. Benchmark in `rs/moq-net/benches` swept over live requests and
  sessions sharing an origin, so the cost grows only with the requests
  renewed. (From the deleted concurrent-renewal quest, which measured this
  against #5148's earlier serial refresh.)
- On moq-transport the update carries the token as USE_VALUE and needs only
  step 1. ✅ Decided 2026-10-19 (maintainer, closing
  [#5927](https://github.com/moq-dev/moq/pull/5927)): lite requests never
  name an auth, so on lite `Route::auth` puts nothing on the wire. A refused
  update ends only that request.
- Mirror `Auth` and `Route::auth` in `js/net`; the bindings expose no auth
  yet, so [Bindings](/quest/m1/auth/bindings.md) carries them.
- Describe moq-transport request renewal in `doc/concept/moq-lite.md`'s
  Authorization section, and in `doc/concept/standard.md`, which describes
  only incoming request tokens.

Public API: a new optional auth on routes in Rust and JS.
Wire: none beyond the existing AUTHORIZATION TOKEN on PUBLISH_NAMESPACE and its
REQUEST_UPDATE.

## Related

- [A request right behind its token](/quest/m1/auth/request-race.md) - lite renewal through the union
