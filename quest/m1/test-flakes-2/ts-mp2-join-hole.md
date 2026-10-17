# [M] TS export loses no MP2 group after its live join

## Goal

TS validation's "TS full multiplex at its own rate" step (`just test ts --full`) passes
reliably because the exporter never skips an MPEG-1 audio group right after it joins.
The fix lands at the cause, with a deterministic regression test that fails without it.

## Plan

Seen 2026-10-17 on #5857's TS validation run 39042472016 (head 754a5af, `main` with
#5854 plus a test-only change). PID 0x101 lost 96 ms in one 120 ms step at 2.390 s,
right after the capture's first MP2 frame. The exporter logged it itself, so this is
real loss, not one of the ffprobe ghost holes #5854 fixed in `holes.py`:
`media skipped upstream ... track="2.mp2" at=2.389977s skipped=120ms`.

What #5854 already rules out: the SUBSCRIBE carried `max_delay: 450ms`, so the
zero-budget race it fixed (and that `a_live_start_loses_no_group_the_publisher_bursts`
reproduces) is not this. The skip was declared about 60 ms after `subscribe started`,
far inside the budget, so a budget expiry can't explain it either. The subscriber
starts before the publisher, so the relay held no backlog at the join. MP2 is
published as one group per 24 ms frame, about five per PES, and 120 ms is about one
PES.

Suspicion, unverified: #5854's live latch (`container::Consumer::live`) starts at the
newest group *announced* when any frame first arrives, and drops what lies below it.
A group's announcement and its frames race on separate streams. So the latch may pick a
start that later reads as a hole against groups still in flight, or that misjudges a
burst.

First step: trace the consumer's skip and latch decisions against that run's artifact
(the `ts-failure` upload, retained until about 2026-10-24) to find which rule declared
the gap. Then reproduce it deterministically over `moq_net::transport::memory`, the way
#5857's test does, before choosing a fix.

Public API: none expected. Wire: none expected.

## Related

- [Audio join budget](/quest/m2/audio-join-budget.md) - moq-audio's live join may share the same race; whichever lands first should check the other
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - loss after a late join is part of the TS contract
