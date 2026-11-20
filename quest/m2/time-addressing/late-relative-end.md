# [M] A relative end on a partial lite-07 copy ends where its publisher's does

## Goal

A reader whose range ends at a relative anchor, joining a lite-07 received
copy that already holds part of the track, reads through the group its
publisher counts back to and no further, in Rust and `@moq/net`. Today the
copy counts the end back over the groups it happens to hold: over a copy
holding groups 1 and 2 of 0..9, a later reader of `[{time:0..relative:1}]`
counts one back from 9 to group 2, so it reads 1, 2, 0 and ends, where it
should read 0 through 8.

## Plan

Found by [moq-dev/moq#9198](https://github.com/moq-dev/moq/pull/9198), which
fixed the same bug for a relative start. Under the maintainer's rule there, a
subscription never changes behavior based on cache state: a start resolves only
from its publisher's report (SUBSCRIBE_OK or SUBSCRIBE_STARTED, kept in
`TrackState::relative_starts` in `rs/moq-net/src/model/track.rs`), and a reader
no report answers asks the copy's upstream for its range and waits (the asks in
`model/track/demand.rs`). An end has no such report: the publisher reports
where each range starts, never where it ends. So #9198 left ends counting over
the cache (`end` in `model/subscription/window.rs`), which breaks the same rule.

- Reproduce first, as `range_anchors.rs` and `js/net/src/integration.test.ts`
  tests beside `a_later_relative_start_on_a_partial_copy_reads_only_its_groups`.
- Recommendation: carry each range's resolved end beside its start in
  SUBSCRIBE_OK and SUBSCRIBE_STARTED. Lite-07 is still `-wip`, so the wire may
  change freely. The copy then resolves a relative end from that report, as
  `relative_starts` does for starts, and asks for it and waits the way a start
  does.
- Alternative: the copy treats a bounded range as ended once the subscription
  accounts for every group it delivered. That needs no wire change, but the
  copy can't tell a range that ended from one still owed a late group.

## Waiting

- Maintainer: whether lite-07 SUBSCRIBE_OK and SUBSCRIBE_STARTED gain a
  resolved Range End (recommended, since the draft is unpublished), or the
  copy infers the end from accounting with no wire change. An agent can't
  decide a wire change on its own.
