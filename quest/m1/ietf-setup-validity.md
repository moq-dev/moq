# [XS] Every IETF setup wait runs under request validity

## Goal

Every wait while the IETF subscriber sets up a request ends as soon as the
request stops being valid (its route retired, or its grant revoked, narrowed,
or expired), with the same local refusal and peer cancellation that the
SUBSCRIBE's request-ID and SUBSCRIBE_OK waits give since
[#6061](https://github.com/moq-dev/moq/pull/6061). In
`rs/moq-net/src/ietf/subscriber.rs` that covers:

- `Stream::open` for SUBSCRIBE, TRACK_STATUS, the cache-miss group FETCH,
  and the joining FETCH on draft 17+;
- the request-ID wait for a fetch-only TRACK_STATUS;
- the cache-miss group FETCH's request-ID and response waits, whose response
  wait still uses a `retired` flag.

Today each is bounded only by QUIC stream credit or the ten-second
request-ID give-up.

## Plan

Decided with the maintainer on 2026-10-20, from #6061's follow-ups:

- Reuse #6061's poll-native shape: poll the gate and `validity` beside
  `Control::poll_credit(waiter, &mut Deadline)` or the response read inside
  one wait, rather than racing the whole setup future. An allocated ID is
  still sent on drafts 14-16, since the peer grants MAX_REQUEST_ID credit
  back only for requests it receives; the response wait then cancels it.
- Refuse the request locally before awaiting the peer's acknowledgement of
  the cancellation, as #6061 does.
- If [Rust request credit](/quest/m1/rs-request-credit.md) lands first,
  `Stream::open` no longer waits for stream credit, and only the request-ID
  and response waits remain here.

Regression tests on sim time, one per wait, each failing on main by timing
out: a revocation and a route retirement during the wait refuse the request
promptly, on drafts 14 and 16 and on 17+ where the wait differs.

Public API: none. Wire: none.

## Related

- [#6061](https://github.com/moq-dev/moq/pull/6061) - brings the SUBSCRIBE's own request-ID and SUBSCRIBE_OK waits under the grant
- [Rust request credit](/quest/m1/rs-request-credit.md) - a request without stream credit fails at once instead of waiting in `Stream::open`
