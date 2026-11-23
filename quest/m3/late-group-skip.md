# [M] A late group that retransmission recovers in time still plays

## Goal

A media reader with a short `Delay::max` (`rs/moq-mux/src/container/consumer.rs`) plays a group whose frames arrive late but within its lag, instead of skipping it and waiting for the next keyframe.
A group that is truly lost is still given up at `max`, and what of a late group arrives on time still plays rather than going with the skip.
A moq-side test proves both: a group whose frames arrive late within the reader's lag is delivered whole, and one whose frames are late past `max` is given up at `max`, no sooner.
`@moq/hang`'s consumer (`js/hang/src/container/consumer.ts`) mirrors `Delay` and the skip, so a change lands in both.

## Plan

Decided 2026-11-23 (maintainer, /quest-run in moq-dev/studio): ✅ a quest of its own here (rejected: folding it into [expire-by-higher-min](/quest/m3/expire-by-higher-min.md); dropping it).
The studio holds [moq-dev/studio#231](https://github.com/moq-dev/studio/pull/231), which narrows a delayed layer's `Delay::max` to about its lag + 100 ms so a lost group stalls it no longer than it plays behind, until this lands: on a lossy link the narrow max made outage stalls worse.

Evidence from #231's harness (loopback `netem` inside `unshare -rn`, a `moq_relay::test_relay`, two 320x180 30 fps openh264 broadcasts with a 1 s GOP, a layer about 400 ms behind, release builds; "stalled" sums gaps over 100 ms between new pictures per 60 s after a 5 s warmup):

- Random loss (20±5 ms delay, 2% and 5% loss): QUIC retransmits inside the 400 ms lag, with no stall beyond the program's own.
- Outages (1% loss plus 600 ms of 100% loss every 5 s, 5 runs): layer stall over program stall is 0.63 with `max` = 4 s, 0.77 with `max` = lag + 100 ms, 0.69 with lag + 500 ms.
- 1.2 s outages (1 run each): 17.8 s stalled at a 4 s max, 15.8 s at the short max.

The 1.2 s outages go the other way (15.8 s against 17.8 s), but are one run each, so only the 5-run outage rows show the regression.

Hypothesis: the outage's data is retransmitted, so it arrives late but intact; with a short max, the reader skips the late group and waits for the next keyframe, giving up frames a longer max would have played.
Check it before building on it: a group is one stream, which delivers in order, so a frame behind a stalled packet cannot arrive on time, and the cost may instead be which frames the skip drops or when it fires relative to the retransmission landing.

Where the skip lives, read from the code at this commit: `Delay::max` is in `rs/moq-mux/src/container/consumer.rs` (`Delay`), and the reader's skip is by group and by timestamp, not by arrival age.
A blocked group is skipped, its unread frames given up, once the newest content less where the next group begins reaches `max` (`should_skip`, plus an early skip once the group's last frame ends at that start); a missing group is given up by the same measure in moq-net's ordered reader.
`max` sums with the catalog warmup while the reader warms.
A third path skips a group with neither timestamp condition met: once the decoded-frame byte cap is full and the cursor's group stalls, the cursor moves on (the `Consumer` docs' pressure skip); tell it apart from a `max` skip in the reproduction.
What remains open is why that fires early on a lossy link, not its unit.

Recommended order: first reproduce that skip in a test and find what it drops, then check whether [expire-by-higher-min](/quest/m3/expire-by-higher-min.md) already changes it, since it bounds a group that isn't whole by a higher group's lowest timestamp, keeping arrival age as a separate backstop that can still expire a superseded group; if it settles this, make this quest's test that quest's and delete this one.
Reproduce with a deterministic test (a paused clock and a scripted late delivery), not the netem harness.

Public API and wire: unknown until the cause is found; say which in the PR.

## Related

- [A group that isn't whole expires by a higher group's lowest timestamp](/quest/m3/expire-by-higher-min.md) - changes when an incomplete group expires, and may settle this
