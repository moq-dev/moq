# [M] Rust track tail

## Goal

A moq-net subscriber, lite and IETF, accepts every group below a track's
declared end until each is accounted for, even when its stream arrives after
the subscription's end, then ends cleanly. The IETF publisher reports how many
data streams it opened. The rule and the grace match the JS subscriber, so a
reader cannot tell which side it is talking to.

## Plan

Rust already records SUBSCRIBE_END with `finish_at`, which is not terminal,
and its publishers drain their group tasks before they end a subscription. The
remaining gap is the subscriber's bookkeeping:

- Lite: once the subscribe stream FINs, `remove_subscribe` drops the entry,
  so a group stream whose header decodes afterwards fails with
  `Error::Cancel` in `lite/subscriber.rs`. Keep the entry, with its boundary,
  until every group below the boundary is accounted for or the grace expires.
- IETF: retiring the alias on PublishDone makes a late stream fail with
  `Error::Cancel` (`ietf/subscriber.rs`, `Alias::Retired`). Retire it only
  once the streams are accounted for or the grace expires.
- Grace: a group reset before its header arrived can never be accounted for,
  so give up after the same grace as `@moq/net` (`js/net/src/tail.ts`) and end
  cleanly, skipping the missing group as stale: the effective `max_age` as a
  wall-clock stopgap on moq-lite, 1s when it is zero, and 1s on IETF. The
  reliable-reset quest removes both.
- Only stream-delivered groups are waited for; datagrams are never.
- IETF publisher: send the real number of data streams opened in PublishDone
  instead of `stream_count: 0`. On receipt, treat the count as a hint: stop
  waiting once that many are accounted for, but accept a late stream below the
  boundary within the grace, so a published peer's 0 keeps working.
- IETF on drafts 14-22: write the END_OF_TRACK object at the boundary, and
  decode it. `@moq/net` already sends it on its own stream at object 0 of
  the group `final`, after the group streams drain; moq-net's subscriber
  rejects status 0x4 as `Unsupported` today, so it aborts a bogus group
  `final` at the end of every JS-published IETF track until this lands.

`@moq/net` settled the draft reading: on 14-22 Stream Count counts every data
stream opened, fill streams included (20+), with a 2^62-1 or 2^64-1 unknown
sentinel. PUBLISH_DONE carries no end location on any of them, so the end is
the END_OF_TRACK object: at object 0 of group G the track ends at G,
otherwise at G+1. Only TRACK_ENDED (and SUBSCRIPTION_ENDED before 20) ends a
track cleanly; an error status aborts it.

Reproduce each case before fixing it: a group header decoded after the
subscribe stream's FIN, and a late stream after PublishDone, over the mock
session in `rs/moq-net/tests/support`. Add a Rust-JS interop case to
`just test smoke --all` for a publisher that ends a track with a group still
in flight.

## Required

- [lite-07 stream count](/quest/m1/lite-stream-count.md) - the moq-lite accounting this builds on, instead of SUBSCRIBE_DROP

## Related

- [Session death error](/quest/m1/session-death-error.md) - tracks ending wrong when the session dies
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - removes the grace
