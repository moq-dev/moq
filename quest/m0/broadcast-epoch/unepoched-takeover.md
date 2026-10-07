# [M] A better route without an epoch takes over

## Goal

When a path's routes carry no epoch, whichever route `route_order` ranks first
always serves it. A new winner takes over with a hard switch, as a newer epoch
does: subscriptions in flight end, and downstream sessions see the switch as an
announcement (End then Start), even when the new route's metadata matches the
old. Viewers re-request and land on the new route, never stitched across the
two. This holds on every version, lite-07 included, for any route without an
epoch.

So a lite-06 publisher that restarts onto the same path while its old session
lingers reaches viewers within an RTT, instead of leaving them on a dead front
until a reload ([#4970](https://github.com/moq-dev/moq/pull/4970)'s open
decision, and the second case in
[#4999](https://github.com/moq-dev/moq/pull/4999)).

## Plan

Decided 2026-10-07 while re-planning #4999 and #5001:

- Routes without an epoch are no longer sticky. Since
  [#4942](https://github.com/moq-dev/moq/pull/4942), a front without an epoch
  stays on its first route (`pick`'s `Some(None)` arm in
  `rs/moq-net/src/model/origin.rs`, the module docs in `model/front.rs`), so a
  dead route lingers and a new request joins it. Before #4942 a better route
  was re-announced, and this restores that.
- The switch is hard, like a newer epoch's: the old front ends and its tracks
  in flight end, so nothing splices bytes from two publishers. That keeps the
  README's "never stitched" rule while dropping "stays on the worker that
  first served a subscription".
- Any new winner takes over, including one decided by the rendezvous-hash
  tiebreak. Without an epoch a relay can't tell a restarted publisher from an
  extra replica, so there is no narrower rule that fixes restarts.
- Consequence, accepted: any change of winner restarts playback for the paths
  it moves, on every version. That includes a cost change (a GOAWAY drain
  re-price) and a change to an equal-cost pool (a transcoder worker joining, a
  relay peer reconnecting), which moves the paths the hash now gives the new
  route. Routes with an epoch are unchanged.
- A change of winner between routes without an epoch always reaches downstream
  as an announcement, even with identical metadata. Today the announce cursor
  hides it ("a reconnect under an identical route is invisible"). Two routes
  with the same epoch and identical metadata are a seamless failover and stay
  invisible.
- Rejected: `@moq/watch` resubscribing on `Internal` or `SessionClosed`
  (#4999), since players recover on announcements, not errors; re-announcing
  only once the old front dies, since viewers stay blank until the old session
  times out; taking over only on a strictly better route, which leaves an
  equal-cost restart blank; the newest winning ties, which lets a joining
  worker take every tied path; and waiting for lite-07 to be the default.

Tests to flip: `better_route_keeps_the_incumbent` in
`rs/moq-net/tests/route_change.rs`, and `identical_reannounce_is_invisible` in
`origin.rs` for routes without an epoch. `route_dies_without_an_epoch` keeps
its expectation (a dead route ends its subscriptions), but its `standby()`
must price `B` strictly worse, or it takes over before the trigger; extend it
so a re-request lands on `B`. js/net's origin already swaps to a better entry
and closes the old front (`route()` in `js/net/src/origin.ts`), and its
announce cursor emits End and Start on a new winner, so the JS half is likely
tests only.

Wire: no new messages, but a behavior change for other relays.
`drafts/draft-lcurley-moq-lite.md` says a subscription between routes without
an Epoch "stays on its route and ends with it". Replace that with: the relay
ends it when another route wins, and announces the change.

## Related

- [Apps](/quest/m0/broadcast-epoch/apps.md) - #4970 drives `@moq/watch` and `moq play` from announcements, which this relies on
