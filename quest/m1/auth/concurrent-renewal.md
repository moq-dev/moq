# [M] A token refresh renews every live namespace at once

## Goal

Replacing the request token renews all of a session's token-backed
PUBLISH_NAMESPACE advertisements concurrently, as far as the peer's request-ID
credit allows. With enough credit, the last namespace is renewed about one
round trip after the refresh, not one round trip per namespace. The
session's other announce work does not stall behind the renewals. A
benchmark swept over namespaces and sessions shows the refresh cost growing
only with the namespaces touched.

## Plan

Deferred from #5148 by the maintainer, with a benchmark sweep. At #5148's
head (`ed595beef`), `refresh_request_token` (`rs/moq-net/src/ietf/publisher.rs`)
runs inside the publisher's namespace loop. For each token-backed
advertisement in turn, it takes a request ID, writes the update, and waits
for the answer. So with N advertisements the last one is renewed about N
round trips after the refresh. If the old grant lapses inside that window,
the peer ends the late advertisements even though the refresh succeeded.
Meanwhile, new announcements and withdrawals wait behind the whole loop.

Root problem: renewal is awaited inline in the loop that serves every
namespace event. Make each advertisement's renewal its own in-flight step,
polled beside the loop's other events, as #5148 did for subscription renewal.

Keep the contracts #5148 settled:

- At most one renewal in flight per advertisement, superseded by a newer
  token while it waits for credit.
- A refusal or cancel ends only that advertisement and re-offers it with the
  new token.
- Request IDs reach the wire in allocation order. Drafts 14-16 require the
  next sequential ID (`Control::accept`), and a concurrent sender must not
  let a later ID overtake an earlier one, as the review of #5148 found for
  FETCH and subscription renewals.
- Request-ID credit bounds how many renewals start at once. Add no new cap.

Benchmark in `rs/moq-net/benches` (`auth.rs` or `announce.rs`). Sweep live
token-backed namespaces against sessions sharing an origin. A refresh's cost
should grow with the namespaces it renews, not with the other sessions'.
Tests on mocked time with a fixed peer round trip:

- When the peer grants request-ID credit for every live advertisement, every
  renewal answer arrives within about one round trip of the refresh.
- When credit is short, renewals stay within it and go out in allocation
  order. Later ones start only as credit returns, and none bypasses the limit.
- An announcement made during a refresh goes out without waiting for it.

Public API: none. Wire: none, only the timing of the existing updates.

## Required

- [Request tokens](/quest/m1/auth/request-token.md) - #5148 adds the renewal this changes
