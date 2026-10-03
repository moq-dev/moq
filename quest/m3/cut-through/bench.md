# [M] Benchmark head-of-line blocking at a relay hop

## Goal

A nightly bench measures how long a frame takes to complete at the viewer when
the publisher's hop to the relay loses packets, and how much of that a
cut-through relay could win back. It ends with a written go or no-go for the
rest of the [cut-through line](/quest/m3/cut-through/README.md).

## Plan

Decided 2026-09-30: real stacks, not a mock. The loss recovery being measured
belongs to the QUIC stack, so a mock transport with a made-up retransmit model would
partly measure its own assumptions.

- Publisher, then a seeded `moq-shaper` (loss and delay), then `moq-relay` over
  QUIC, then a clean or rate-limited hop to the viewer. Reuse the shaper wiring
  from `rs/moq-relay/tests/drills.rs`.
- Sweep loss rate, frame size (audio-sized through a large I-frame), and egress
  headroom (egress rate relative to the media rate).
- Report per-frame completion at the viewer, from publish to last byte, p50 and
  p99. The opportunity is the drain time on this same two-hop path: per frame,
  the time from the relay's ingress hole filling to the frame completing at the
  viewer, minus the same for frames with no hole. That excess is the burst
  cut-through removes. No direct publisher-to-viewer control: the gain exists
  only with a relay in the middle, and one QUIC hop already reassembles out of
  order, so a direct run measures a different topology (review of #4616).
- Split the held bytes three ways: bytes in the frame that has the hole, bytes
  in later frames whose headers were known while the hole was open, and bytes
  behind a lost header. Only the first two are forwardable; the last blocks with
  or without cut-through and is reported but excluded from the opportunity. The
  first split shows what [multiple in-flight frames](/quest/m3/cut-through/frames.md)
  add beyond the current frame. Report the drain estimate as an upper bound, and
  include both a payload-loss and a header-loss case to validate the split.
- Take publisher and subscriber counts as parameters, so
  [relay cut-through](/quest/m3/cut-through/relay.md) reuses the rig for its
  fan-out sweep.
- Record the loss-delay counter from the same runs, if
  [Loss delay](/quest/m3/cut-through/loss-delay.md) has landed, so production
  numbers can be read against the lab.
- It runs on wall-clock time, so it is a nightly bench, not a unit test. Wire it
  into the nightly workflow.

Write the numbers and the verdict into this line's README. The maintainer makes
the go or no-go call from them; no threshold is set in advance. On a no-go,
delete the build quests and keep the bench.
