# [M] The transport drills run over a seeded, impaired UDP path

## Goal

The transport drills run against a bidirectionally impaired QUIC path (delay,
jitter, loss, reorder, and a rate limit) with the impairment asserted rather
than assumed, on macOS and Linux alike, with no capabilities and nothing
touching the host's network. A profile that fails to install fails the run.

## Plan

`rs/moq-relay/tests/drills.rs` already owns the scenarios: cancellation
under backpressure, relay death mid-group, and republish after an
interrupted publisher. They drive real QUIC over `127.0.0.1` inside one
process, relay and clients alike, so the impairment can be a userspace UDP
shaper the test itself owns: the client connects to the shaper's socket,
which forwards each datagram to the relay after the profile's treatment, and
back. That is not an HTTP interceptor or a TCP proxy, which cannot impair
QUIC; it is a datagram relay, and QUIC is indifferent to the extra hop.

- A `Shaper` in the drill support code: per-direction delay and jitter,
  loss, reorder, and a token-bucket rate limit, all driven by one seeded RNG
  so a failing run's seed reproduces the same treatment. Kernel scheduling
  still varies delivery timing; the seed makes the decisions reproducible,
  not the clock.
- Assert the impairment applied before grading a drill: the shaper counts
  what it dropped, delayed, and reordered, and a profile that treated nothing
  fails the run. A profile that silently did nothing turns an impaired run
  into an unimpaired pass.
- Record the profile, the seed, and the shaper's counters with the run's
  artifacts.
- The drills pass unchanged under a moderate profile; add the impaired
  variant as a second lane of each drill rather than a separate suite, so a
  scenario cannot drift between the two. No retries to make an intermittent
  failure green; a drill that only passes unimpaired is a finding.

Kernel-real impairment (`netem` in a private network namespace) is out of
scope: it is Linux only, needs `CAP_NET_ADMIN`, and the drills test the
protocol's reaction to loss and delay, not the kernel's rendering of them.

## Related

- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - stores profiles, seeds, and traces
