# [XS] Resetting a kio sleep to its deadline does not wake it

## Goal

Resetting a kio sleep to the deadline it already has leaves the parked task
asleep, on the simulated and `Manual` clocks, so a caller that resets on
every poll no longer spins the simulator.

## Plan

Decided 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8139](https://github.com/moq-dev/moq/pull/8139), whose relay
latency code reset its sleep on every poll and spun the simulator.

`Timer::reset` in `rs/kio/src/clock/sim.rs` and `rs/kio/src/clock/installed.rs`
always disarms the entry and wakes its waker, so the parked task re-polls,
resets again, and loops without the clock advancing. The host backend
(tokio's `Sleep::reset`) doesn't wake, so only tests see it. When the new
deadline equals the armed one, keep the entry and skip the wake; a different
deadline still wakes, so the task re-arms. Test on both clocks that a task
resetting to the same deadline on every poll is polled a bounded number of
times and completes at that deadline, and that a reset to an earlier
deadline from another task completes at the earlier one.

Public API: none. Wire: none.
