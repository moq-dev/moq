# [S] Back off an unreachable probe target

## Goal

A session whose probes keep failing to raise the estimate toward its target
backs the target off, instead of spending about 5% of its media on duplicate
probe bytes indefinitely.

## Plan

- Measured in [moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635):
  4.6% redundant bytes for a 100 Mbit/s target on a 3 Mbit/s path.
  `rs/moq-quic/src/connection/bandwidth_probe.rs` keeps no state about failed
  rises.
- The cost grows with the round trip, since each filled ProbeBW_UP lasts a few
  rounds: about 15% of the media at 200ms. Now that BBR3's app-limited
  estimate ages, a target the path lost is retried too, and `rtt_200ms` in
  `rs/moq-quic/src/tests/probe.rs` spends 14% against its 10% overhead budget
  on constant-bitrate media.
- Keep the requested target (`set_probe_target`) apart from the effective one
  the probe paces to. Back off the effective target exponentially after
  ProbeBW_UP cycles with no estimate rise, reset it to the requested target
  when the caller changes that, and retry the requested target on the same
  backoff so returning capacity is found. Prove it on a bufferbloat path.
