# [M] A floor below a resolved copy reaches upstream

## Goal

A relay fetches upstream the groups a new reader asks for with an absolute
floor below where its copy already resolved. Today it never does: once a
reader asking for the oldest group (`Range::oldest`, or a relative start)
holds a copy, the demand union folds every other range into "oldest"
(`rs/moq-net/src/model/track/demand/ranges.rs`), so a later reader's floor at
group 5, under a copy resolved at group 10, sends no SUBSCRIBE_UPDATE.
Upstream already moved its cursor past 5 and never offers it, even while it
still caches 5 through 9 and the new reader's max age covers them. The
reader silently starts later than it asked, and only through a warm relay.

An archive resuming at its last stored frame through a relay that a viewer
already warmed hits this, as does any absolute range start.

## Plan

Found while building SUBSCRIBE_ALL
([moq-dev/moq#5931](https://github.com/moq-dev/moq/pull/5931)), which
reproduced it with a throwaway test and fixed it only for listed tracks
(`listed_demand` in `f074d09e7`) by turning a listed track's demand into an
absolute floor at its resolved start.

Decided by the maintainer on 2026-10-19, judged on behavior: compare-only. A
resolved "oldest" start is information about the copy, not a filter.

- **Aggregation.** An "oldest" reader that resolved at S counts as S, so the
  copy's demand is min(S, every other reader's floor), and a newcomer at
  F < S sends F upstream.
- **Delivery.** Nothing filters the existing reader, so a group below S that
  arrives late still reaches it within its max age.

Rejected: #5931's absolute floor at the resolved start, which drops those
late groups. The lite draft's SUBSCRIBE_OK already says its group "is not a
new floor" and that a later group below it is still delivered, which matches
this, so the draft does not change.

Lands after [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929),
which makes a lowered floor re-offer cached groups at the publisher (decided
2026-10-19). This quest gets the lower floor to the upstream hop; #5929 makes
that hop serve it. Each is a no-op for this bug without the other. #5929 also
moves relative-start resolution into the model (`track::Subscriber::floor()`),
which is the natural place to learn that a start resolved.

Watch for:

- **Every relay changes.** This alters the upstream SUBSCRIBE_UPDATE stream
  of every relayed track. Make sure a reader resolving does not, by itself,
  send an update per subscriber; extend `track_subscriber_churn` in
  `rs/moq-net/benches/track.rs` with resolving readers, swept over subscriber
  count.
- Check whether `js/net` aggregates demand the same way and needs the same
  fix.

Test on mock time through a relay: a reader with an absolute floor below a
copy an "oldest" reader resolved gets the groups upstream still caches (fails
on `main`); the same through a cold relay gives the same answer; a late group
below a resolved start is still delivered within max age; and a reader
resolving alone sends no SUBSCRIBE_UPDATE.

Public API: none. Wire: no encoding change; a relay now sends a lower
SUBSCRIBE_UPDATE floor upstream in the case above.

## Related

- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - #5929 lands its serving half, including the cached-group re-offer this relies on
- [Archive producer](/quest/m1/archive/producer.md) - its resume floor crosses relays that viewers may have warmed
