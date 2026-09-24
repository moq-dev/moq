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
  so give up after the subscription's effective `max_age` and end cleanly,
  skipping the missing group as stale. The reliable-reset quest removes this
  wait once headers survive a reset.
- IETF publisher: send the real number of data streams opened in PublishDone
  instead of `stream_count: 0`. On receipt, treat the count as a hint: stop
  waiting once that many are accounted for, but accept a late stream below the
  boundary within the grace, so a published peer's 0 keeps working.

Confirm Stream Count's meaning for the implemented IETF drafts first, as the
JS quest does; both must agree.

Reproduce each case before fixing it: a group header decoded after the
subscribe stream's FIN, and a late stream after PublishDone, over the mock
session in `rs/moq-net/tests/support`. Add a Rust-JS interop case to
`just test smoke --all` for a publisher that ends a track with a group still
in flight.

## Related

- [JS track tail](/quest/m1/js-track-tail.md) - the same rule in `@moq/net`
- [Session death error](/quest/m1/session-death-error.md) - tracks ending wrong when the session dies
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - removes the `max_age` grace
