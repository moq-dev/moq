# [M] A waiting time start reads its owed group, and a fill reaches subscriptions

## Goal

A Rust reader whose time start is still waiting on the groups held never hands
out a later group ahead of the one the start resolves to, and a group a
publisher accepts from a fill is delivered on its live subscriptions the way
`@moq/net` delivers it, or the difference is documented as intended.

## Plan

Both were seen while chasing the interop hang that
[moq-dev/moq#9127](https://github.com/moq-dev/moq/pull/9127) fixed, through a
lite-07 publisher, a relay, and two viewers, all in one simulated test:

- A viewer that fetched group 67, then subscribed from `Time(66)` before 66
  was written, read 67 from an `Ordered` reader at once, then reported 66 as an
  `Old` gap once 66 landed, though its budget was unlimited. Its own copy held
  67 and placed nothing for the time, so the waiting span read from 66, and the
  ordered read did not wait for the group the start would resolve to.
- With the fetch on the other viewer instead, the subscribing viewer got 66 and
  never 67: neither the Rust publisher nor the relay served the fill-accepted
  group on a subscription, while the `@moq/net` publisher serves one there
  (the interop relay received 69 over its subscription in every passing run).

Start from a test in `rs/moq-net/tests/integration` with that topology
(`MockConnectOptions` on `moq-lite-07-wip`, the publisher's `Requests` serving
67 only). Decide the second from the draft's Subscribe section: a fill below
the live edge is history, but one at or past it is the live feed.

Public API: none expected. Wire: none.
