# [M] JS subscribers FETCH over moq-transport

## Goal

`@moq/net`'s moq-transport subscriber fetches a past group with a standalone
FETCH, so `track.Consumer.fetchGroup()` works on moq-transport. JS subscribers
join the CLI FETCH interop round beside Rust.

## Plan

Found by [moq-dev/moq#7015](https://github.com/moq-dev/moq/pull/7015). Today
`track.Consumer.fetchGroup()` is rejected on moq-transport. The mid-group
join already recovers the current group's head, with a joining FETCH on
draft 18 or a fill on draft 22
([moq-dev/moq#7089](https://github.com/moq-dev/moq/pull/7089)), and its
fetch-stream decoder and head stitching are what a standalone FETCH reuses.

Decided 2026-10-28 (maintainer): one [M] quest in m1, after
[JS ranges](/quest/m1/subscribe-ranges/js.md), which turns `fetchGroup` into
a one-range subscription. Map a past range to a standalone FETCH, the way
Rust's subscriber does. Then add the native JS runtimes as subscribers to the
`run_fetch_round` cells in the tail suite. Our publishers answer only one
group ([one group](/quest/m1/ietf-publisher-one-group.md), 2026-10-28), so a
past-range FETCH against them is refused. Test ranges against a third-party
publisher, and one group against ours.

Public API: none beyond JS ranges' surface. Wire: none.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the one-range subscription this maps onto FETCH
