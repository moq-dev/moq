# [M] Live requests follow their auth

## Goal

A publisher or subscriber renews the credential behind a live request
without reopening it. A `Route` or `Subscription` holds an optional
`moq_net::Auth`; when the application swaps it, the session sends
REQUEST_UPDATE (moq-transport) naming the new auth; on lite a `Route` renews
via ANNOUNCE_UPDATE and a `Subscription` via SUBSCRIBE_UPDATE. The request
stays up. The first consumer is a client that keeps a
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
  `Option<moq_net::Auth>` (name to be settled), and changing it triggers the
  update automatically. Unset means the session's scope.
- ✅ Decided 2026-10-15 (maintainer): outgoing tokens go through `add()`;
  #5148's `set_request_token` and its renewal sender are not carried forward.
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
- PUBLISH_NAMESPACE renewal comes first. Per-SUBSCRIBE tokens follow the same
  shape but have no consumer yet; include them only if they fall out of the
  same code.
- On moq-transport the update carries the token as USE_VALUE and needs only
  step 1, so the PUBLISH_NAMESPACE consumer doesn't wait on the lite wire. On
  lite it names the auth registered through
  [Requests name their auth](/quest/m1/auth/request-auth-id.md), so the lite
  path follows that quest. A refused update ends only that request.
- Settle the type and field names with the maintainer before implementing;
  mirror them in `js/net` and the bindings.
- Describe request renewal in `doc/concept/moq-lite.md`'s Authorization
  section, and in `doc/concept/standard.md`, which describes only incoming request
  tokens; the moq-transport path can land before the lite one.

Public API: a new optional auth on routes and subscriptions in Rust and JS.
Wire: none beyond existing REQUEST_UPDATE, SUBSCRIBE_UPDATE and ANNOUNCE_UPDATE,
with lite's auth field from Requests name their auth.

## Related

- [Requests name their auth](/quest/m1/auth/request-auth-id.md) - the lite path's prerequisite; moq-transport renewal doesn't wait on it
