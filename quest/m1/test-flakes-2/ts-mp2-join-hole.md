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

Evidence kept from that run's `ts-failure` artifact, which expires about 2026-10-24
and holds only INFO logs, not the consumer's decisions:

- Publisher instance up at 15:04:31.10. The subscriber subscribed to `2.mp2` at
  15:04:33.082 with `max_delay: 450ms`, `start_group: None`, `order: Descending`. The
  relay forwarded the same upstream at 15:04:33.083.
- The skip was logged at 15:04:33.143, about 60 ms after subscribing. The capture's
  first MP2 frame is at 2.366 s against a source starting at 1.070 s. Video starts at
  2.080 s, AC-3 at 2.515 s, both without holes.
- MP2 is published as one group per 24 ms frame, about five per PES, so the 120 ms
  step is about one PES.

What #5854 already rules out: the zero-budget race it fixed (reproduced by
`a_live_start_loses_no_group_the_publisher_bursts`), since this SUBSCRIBE carried the
real budget. What it doesn't rule out: the consumer measures `max_delay` in media time
(`max_timestamp - next_start`), not wall time, so 60 ms after subscribing is no bound.
The publisher had run about two seconds before the join, so its cache could hand the
budget a backlog spanning that much media time even though the relay held none.

Suspicion, unverified: #5854's live latch (`container::Consumer::live`) starts at the
newest group *announced* when any frame first arrives, and drops what lies below it.
A group's announcement and its frames race on separate streams, and the backlog
arrives beside the live groups. So the latch, or the budget's skip rules right after
it, may misjudge a burst that is still in flight.

First step: reproduce it deterministically over `moq_net::transport::memory`, the way
#5857's test does. Use a publisher holding a backlog when the live export joins, then
bursting a PES of one-frame groups. Trace which skip or latch rule declares the gap
before choosing a fix.

Public API: none expected. Wire: none expected.

## Related

- [Audio join budget](/quest/m2/audio-join-budget.md) - moq-audio's live join may share the same race; whichever lands first should check the other
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - loss after a late join is part of the TS contract
