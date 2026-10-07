# [M] A better route without an epoch takes over

## Goal

When a path's routes carry no epoch, the route that ranks best (cost, then
newest) always serves it. A better route takes over with a hard switch, as a
newer epoch does: subscriptions in flight end, and downstream sessions see the
switch as an announcement (End then Start on lite-06 and moq-transport), even
when the new route's metadata matches the old. Viewers re-request and land on
the new route, never stitched across the two. This holds in moq-net and in
js/net's origin.

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
- A change of winning route always reaches downstream as an announcement, even
  when the metadata is identical. Today the announce cursor sends nothing in
  that case, which leaves a client holding the old handle.
- Consequence, accepted: a cost change between live routes without an epoch
  (a GOAWAY drain re-price, a cluster cost shift) restarts lite-06 playback.
  Routes with an epoch are unchanged.
- Rejected: `@moq/watch` resubscribing on `Internal` or `SessionClosed`
  (#4999), since players recover on announcements, not errors; re-announcing
  only once the old front dies, since viewers stay blank until the old session
  times out; and waiting for lite-07 to be the default.

Flip `better_route_keeps_the_incumbent` and `route_dies_without_an_epoch` in
`rs/moq-net/tests/route_change.rs` to expect the takeover, and cover the same
in js/net. Check the lite draft for any text describing a sticky route. Wire:
no new messages.

## Related

- [Apps](/quest/m0/broadcast-epoch/apps.md) - #4970 drives `@moq/watch` and `moq play` from announcements, which this relies on
- [Remove `--hop`](/quest/m0/broadcast-epoch/hop-removal.md) - drops the per-session hop stamp, so restarted routes often have identical metadata
