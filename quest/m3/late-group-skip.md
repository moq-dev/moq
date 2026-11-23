# [M] A late group that retransmission recovers in time still plays

## Goal

A media reader with a short `Delay::max` (`rs/moq-mux/src/container/consumer.rs`) plays a group whose frames arrive late but within its lag, instead of skipping it and waiting for the next keyframe.
A group that is truly lost is still given up at `max`, and what of a late group arrives on time still plays rather than going with the skip.
A moq-side test proves both: a group whose frames arrive late within the reader's lag is delivered whole, and one whose first frames are late past `max` loses no on-time tail beyond what the skip's unit requires.

## Plan

Decided 2026-11-23 (maintainer, /quest-run in moq-dev/studio): ✅ a quest of its own here (rejected: folding it into [expire-by-higher-min](/quest/m3/expire-by-higher-min.md); dropping it).
The studio holds [moq-dev/studio#231](https://github.com/moq-dev/studio/pull/231), which narrows a delayed layer's `Delay::max` to about its lag + 100 ms so a lost group stalls it no longer than it plays behind, until this lands: on a lossy link the narrow max made outage stalls worse.

Evidence from #231's harness (loopback `netem` inside `unshare -rn`, a `moq_relay::test_relay`, two 320x180 30 fps openh264 broadcasts with a 1 s GOP, a layer about 400 ms behind, release builds; "stalled" sums gaps over 100 ms between new pictures per 60 s after a 5 s warmup):

- Random loss (20±5 ms delay, 2% and 5% loss): QUIC retransmits inside the 400 ms lag, with no stall beyond the program's own.
- Outages (1% loss plus 600 ms of 100% loss every 5 s, 5 runs): layer stall over program stall is 0.63 with `max` = 4 s, 0.77 with `max` = lag + 100 ms, 0.69 with lag + 500 ms.
- 1.2 s outages (1 run each): 17.8 s stalled at a 4 s max, 15.8 s at the short max.

Hypothesis: the outage's data is retransmitted, so it arrives late but intact; with a short max, the reader skips the late group, and with it the group of pictures' tail that would have been on time, so the decoder waits up to a GOP for the next keyframe.

Recommended order: first find the skip's unit and its clock (group or frame, arrival age or timestamp) in moq-net and moq-mux, then check whether [expire-by-higher-min](/quest/m3/expire-by-higher-min.md) already changes it, since it bounds a group that isn't whole by a higher group's lowest timestamp rather than arrival age; if it settles this, make this quest's test that quest's and delete this one.
Reproduce with a deterministic test (a paused clock and a scripted late delivery), not the netem harness.

Public API and wire: unknown until the cause is found; say which in the PR.

## Related

- [A group that isn't whole expires by a higher group's lowest timestamp](/quest/m3/expire-by-higher-min.md) - changes when an incomplete group expires, and may settle this
