# [M] User-space TCP beats the kernel baseline, or the epic stops

## Goal

A report compares QMux egress over the user-space stack with kernel TCP
carrying the `TCP_NOTSENT_LOWAT` baseline ([moq-dev/moq#9242](https://github.com/moq-dev/moq/pull/9242)),
over the same bufferbloated paths. It covers bytes of reset or expired streams
that still reach the viewer, frame latency p50/p95/p99, and the session's
`estimated_send_rate` against the path's capacity through a step down and back
up. It also includes a fan-out benchmark that sweeps connections against
bitrate, reporting CPU per delivered byte and memory per connection.

The relay wiring starts only if the stack wins on stale bytes or on estimate
quality without losing on latency. A no-win deletes the epic's remaining quests and
records the numbers in this epic's Plan.

## Plan

- Bufferbloat, not random loss: a netns veth with a deep `tbf` queue, stepped
  like moq-shaper's `cellular-step`, which only shapes UDP. Build the TCP
  harness here, in `bench/` beside `just bench`, and run its kernel arm too:
  [moq-dev/moq#9242](https://github.com/moq-dev/moq/pull/9242) measured the baseline with a scratch harness only
  (its `## Validation` has the setup and numbers).
- Media like a relay viewer's: groups with keyframes, and expiry that resets
  late groups, so stale bytes have something to count.

## Required

- [TCP connection](/quest/m5/userspace-tcp/engine.md) - the stack under test
- [AF_XDP backend](/quest/m5/userspace-tcp/xdp.md) - its packet I/O
- [QMux pull](/quest/m5/userspace-tcp/qmux.md) - the late commit being measured

## Hosts

- linux - AF_XDP and the netns harness
