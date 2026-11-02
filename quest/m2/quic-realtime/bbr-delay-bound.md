# [M] BBR3 delay bound

## Goal

A BBR3 variant in `moq-congestion` caps inflight and pacing when the forward delay
gradient from receive timestamps shows queue growth, and reports its queuing
delay, throughput, and reaction time on the shared moq-shaper uplink presets.

## Plan

The smallest candidate: it keeps BBR3's model, startup, and fairness, and only
adds a queue-delay brake. The comparison must show whether BBR3's ProbeBW
up-cycles still push transient queues the brake cannot stop; that is the case
against it.

- Build it as a configuration of `moq-congestion`'s BBR3, not a fork of the module,
  so a loss leaves nothing to delete but the option.
- Keep BBR3's `Controller::probes` and `probe_inflight` working under the
  brake, so moq-quic's native probing still raises the estimate (decided 2026-10-25, maintainer;
  see the [epic](/quest/m2/quic-realtime/README.md)). Measure that a stopped
  rung recovers under it.
- Unit tests drive the controller with mocked time, not wall clock.
