# [M] A lower-ranked group waiting for stream credit holds back higher-ranked ones

## Goal

Out of stream credit, a Rust publisher's subscription never holds a
higher-ranked group back behind a lower-ranked one waiting for credit: a
newer group that would reset a lower stream reaches the open line even while
another group of the subscription waits there.

Today `TrackRun` (`rs/moq-net/src/lite/publisher.rs`) keeps one group waiting
for credit in `opening`, and its cursor hands out nothing while one waits.
A group that ranks below every stream the subscription holds can never make
room, so it blocks the cursor until credit returns or a stream finishes. With
one credit and descending order, unfinished group 2 holds the stream, late
group 0 waits in `opening`, and newer group 3, which would reset group 2,
never reaches the line. This reproduces on lite-06 at `4265dc5b57`, before
any lite-07 YIELDED: a test that publishes groups 2 (unfinished), 0, then 3
times out waiting for group 3.
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545) handles one case,
a lite-07 continuation waiting in `opening`, by letting the cursor hand out
one more group (`TrackRun::stashed`); a late lower-ranked group stashed there
blocks the cursor again.

## Plan

Recommended: while a group waits in `opening`, keep polling the cursor, and
keep the higher-ranked of the waiting group and the one it hands out
(`Paused::outranks`). The other leaves the line: a lite-07 continuation goes
back to `paused`, and a new group is given up as a group expired under the
subscription's budget is (a lite-07 stale drop, which the next
SUBSCRIBE_UPDATE may re-offer). This keeps one waiting group per
subscription plus the paused continuations stream credit bounds, and folds
in #8545's `stashed`. Check whether `SubscribeAccounting.tla`'s accounting
rules still hold, since a handed-out group may now end in a stale drop
without a stream.

JS (`js/net/src/lite/publisher.ts`) opens every group concurrently, ranked by
send order, so it has no counterpart; confirm with the same test there.

Tests: the repro above, on lite-06 and lite-07, in
`rs/moq-net/tests/integration/stream_credit.rs`.

Public API: none. Wire: none.
