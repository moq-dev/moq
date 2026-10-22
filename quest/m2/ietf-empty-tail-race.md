# [S] An empty tail waits for a head still being written

## Goal

On drafts 14 to 19, a joined group whose head is still arriving on the
joining FETCH when a clean PUBLISH_DONE settles is delivered whole, never as
an empty group. `tail rust -> rust` on a pinned moq-transport pass stops
failing with "empty group".

## Plan

Seen once on draft 16 on 2026-10-22, while debugging
[moq-dev/moq#6407](https://github.com/moq-dev/moq/pull/6407). The relay
subscribed upstream with Largest at `{3, 0}`, so the joining FETCH carries
object 0 of group 3 and the subscription's stream for group 3 has no objects.
PUBLISH_DONE arrived 5 ms after SUBSCRIBE_OK, and the downstream reader got
group 3 with no frames.

Suspected race in `rs/moq-net/src/ietf/subscriber.rs`: the empty tail stream
waits in `claim_fill` while the fill is `Fill::Active`. The settle after
PUBLISH_DONE calls `Fill::end`, which overwrites `Active` with `Ended`, so the
waiting tail sees no head, takes the `None` arm of `open_group`, and creates
group 3 empty. The head then lands on `Fill::Ended` and has no group left to
become. Reproduce it in a unit test first (both stream orders, mocked time),
then make the empty tail and the late head end the same way: the head is the
whole group.

Public API: none. Wire: none.

## Related

- [Headless moq-transport groups](/quest/m2/ietf-headless-group.md) - the
  mid-group stream with no head at all
- [Join after a quick finish](/quest/m2/ietf-join-after-finish.md) - the
  joining FETCH that arrives after the subscription ended
