# [S] An empty tail stream for a headless moq-transport group creates no group

## Goal

When a moq-transport subscription's fill for the Largest Group fails, an
empty subgroup stream (no objects) for that headless group is dropped like a
mid-group stream with objects, in either arrival order, instead of creating
an empty group that reads as complete.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7322](https://github.com/moq-dev/moq/pull/7322) (merged),
which finished the headless-groups quest.

[moq-dev/moq#7322](https://github.com/moq-dev/moq/pull/7322) drops a headless group's mid-group stream that carries objects,
whichever of the stream and the failed fill arrives first, and keeps the
failure as a watermark across fill replacements. It left an empty stream
alone: it still creates an empty group in both orders, and a moq-lite
subscriber downstream of the relay sees a finished, empty group where the
head was lost. Route the empty stream through the same headless check (the
watermark and the parked-tail path in `rs/moq-net/src/ietf/subscriber.rs`),
and add both orders beside `a_headless_tail_after_the_fill_settles_is_dropped`
and `a_headless_tail_before_the_fill_settles_is_dropped`. Streams for
groups the fill didn't owe are unchanged.

Public API: none. Wire: none; behavior only.
