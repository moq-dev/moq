# [M] Container consumers buffer by backpressure

## Goal

A hostile publisher cannot make a container consumer hold unbounded decoded
frames. `@moq/hang`'s `container/consumer.ts` and moq-mux's
`container/consumer.rs` decode ahead at most a byte budget of frames; past
it, reads wait, and the undecoded data stays in the net cache, which the
pool already bounds. Many frames packed into one timestamp, or many groups
in flight, no longer grow the consumer.

## Plan

Decided 2026-10-27 (maintainer), from
[moq-dev/moq#6903](https://github.com/moq-dev/moq/pull/6903): bound by
backpressure in Rust and JS together, not a frame-count cap that refuses a
group. Release keeps the gap until the next cut; no backport.

Today both consumers bound only by timestamp span:

- JS pushes every decoded sample (`group.frames.push` in
  `js/hang/src/container/consumer.ts`), and `#checkMaxDelay` measures the
  span across at least two groups, so frames that share a timestamp have
  zero span and are never skipped.
- Rust buffers a `VecDeque<(Frame, Instant)>` per group
  (`rs/moq-mux/src/container/consumer.rs`) bounded by span and elapsed
  time only.
- The net layers cap a group (32 MiB, 8192 frames) but not how many groups
  a consumer holds, and one CMAF wire frame can decode to many samples.

Steps:

- Charge decoded frames to a per-consumer byte budget and stop pulling from
  net groups while it is full, so pressure reaches the net cache and QUIC
  flow control instead of a refusal. Keep the existing span skip for
  latency.
- Mirror names across Rust and JS.
- Test a publisher that packs many frames into one timestamp, and one that
  opens many groups: memory stays near the budget and playback still skips
  to the live edge.

Public API: the consumer config may gain a budget field; mirror it in JS.
Wire: none.
