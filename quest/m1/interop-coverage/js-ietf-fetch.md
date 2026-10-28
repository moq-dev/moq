# [M] JS subscribers FETCH over moq-transport

## Goal

`@moq/net`'s moq-transport subscriber fetches a past group, and recovers the
start of the current group when it joins mid-group, so a strict third-party
relay no longer cuts a JS player's join to the next group boundary. JS
subscribers join the CLI FETCH interop round beside Rust.

## Plan

Found by [moq-dev/moq#7015](https://github.com/moq-dev/moq/pull/7015). Today
`track.Consumer.fetchGroup()` is rejected on moq-transport, and a JS
subscriber asks for neither a joining FETCH (draft 18) nor a fill (draft 22);
see the SUBSCRIBE filter comment in `js/net/src/ietf/subscriber.ts`. The
in-tree relay replays the group, so only third-party relays show the cut.

Decided 2026-10-28 (maintainer): one [M] quest in m1, after
[JS ranges](/quest/m1/subscribe-ranges/js.md), which turns `fetchGroup` into
a one-range subscription. Map a past range to a standalone FETCH, and a
mid-group join to a joining FETCH on draft 18 or a fill on draft 22, the way
Rust's subscriber does. Then add the native JS runtimes as subscribers to the
`run_fetch_round` cells in the tail suite, and a late-join case through a
relay that does not replay the group.

Public API: none beyond JS ranges' surface. Wire: none.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the one-range subscription this maps onto FETCH
