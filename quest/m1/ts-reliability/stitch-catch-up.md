# [M] A demoted subscriber priority reaches the publisher

## Goal

A mid-stream `track::Subscriber::update` that lowers a subscription's
priority reaches the publisher's aggregate for that track, on lite-07 and
moq-transport, directly and through a relay, including on an old instance
after an epoch switch. A stitched export can then demote its old leg while
the replacement acquires.

## Plan

The latency half of this quest landed in
[moq-dev/moq#7049](https://github.com/moq-dev/moq/pull/7049): a stitched
switch now drops the old program's tail and starts the replacement at
`--delay`. What remains is the priority observation from
[moq-dev/moq#6919](https://github.com/moq-dev/moq/pull/6919), which #7049
reproduced as a propagation bug.

Reproduction (in `rs/moq-net-tests/tests/integration/restart.rs`, with its
`link`, `subscribe`, `write`, `read` and `epochs` helpers): publish `live`
with one `video` track, subscribe from a viewer origin, read a group, call
`update(subscription().with_priority(-2))` on the subscriber, settle, write
and read another group, then read the publisher's
`track::Producer::subscription().priority`. Results from #7049:

| Case | Demotion (-2) | Promotion (+1) |
| --- | --- | --- |
| lite-07, direct or relayed, no switch | reaches the publisher | reaches it |
| lite-07 after an epoch switch (`expect_replaced` in place), direct or relayed | stays 0 | reaches it |
| moq-transport-18, direct, no switch | stays 0 | reaches it |
| moq-transport-18 after an epoch switch, relayed | stays 0 | stays 0 |

The aggregate takes the `max` of its subscribers' priorities, so a demotion
that never arrives looks the same as one masked by another subscriber at 0.
Temporary instrumentation of the publisher's per-priority counts showed:

- moq-transport-18, no switch: the publisher counts two subscribers at 0 for
  the viewer's one subscription, so one SUBSCRIBE (or something beside it)
  holds a second priority-0 subscription. Find it, and either have it carry
  the request's priority or keep it out of the priority aggregate.
- lite-07 after a switch: the publisher counts one subscriber, still at 0,
  and promotion arrives, so the viewer side never sends the demotion. Its copy
  of the old instance's track likely holds a second priority-0 subscriber
  after the restart, masking the demotion in the viewer's aggregate.

Check the relay-and-switch moq-transport case once both are fixed: it also
drops a promotion, so a third cause may remain there.

Land the reproduction as integration tests over both versions, with and
without a relay and a switch. Then decide whether the stitched export should
demote its old leg while the replacement acquires (#6919 measured the
replacement's keyframe arriving 0.7 to 2.2 s after the follow ran out on a
160 kbit/s link); if so, add it and check the bufferbloat fixture's gap.

Public API: none expected. Wire: none.
