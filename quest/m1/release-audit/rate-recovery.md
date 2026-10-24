# [M] Finish bitrate recovery when the bandwidth grant goes quiet

## Goal

After congestion clears, an encoder driven by a bandwidth grant climbs back
to its ceiling even when the grant stops changing. This covers
moq-video's capture driver, the moq-ffi video producer, and the moq-c video
producer. Today each one stalls partway through the ramp until the next
grant change, which may never come.

## Plan

P2 correctness, reproduced. Found in the 2026-10-22 binding and OBS audit at
main [1f5caa7026](https://github.com/moq-dev/moq/commit/1f5caa7026). In a
paused-time reproduction, a 4 Mbps ceiling got a 1 Mbps grant, and 4 Mbps was
restored one second later. The encoder reached 1.25 Mbps and stayed there for
another 60 simulated seconds.

`moq_mux::rate::Control::update` only moves the target when it is called. It
ramps raises at `RAMP` per second since the last applied change. Every caller
only calls it when `bandwidth::Consumer::changed()` fires:
`next_estimate`/`apply_estimate` in `rs/moq-video/src/encode/producer.rs`,
`follow_reservation` in `rs/moq-ffi/src/video.rs`, and its copy in
`rs/moq-c/src/video.rs`. Once the grant settles, no further event arrives. The
moq-video and moq-ffi loops are inherited from release; the moq-c loop is new
on main. OBS's per-packet controller (`cpp/obs/src/moq-rate.h`) is not
affected.

Decided with the maintainer on 2026-10-22:

- Keep `Control` pure. Add a method like `deadline() -> Option<Instant>`
  that returns when the ramp next allows a raise toward the last desired
  target, and `None` once the target settles. Each caller adds a
  `sleep_until` arm on the clock it already passes to `update`
  (`moq_net::time` or `moq_clock`) and calls `update` again with the last
  estimate. `moq_net::time` re-exports only `Instant` and `now` from
  moq-clock, so re-export `sleep_until` there rather than giving moq-c a
  direct moq-clock dependency. Gate it on `feature = "async"`, like
  `moq_net::time::run`: without that feature moq-clock has no runtime, and
  `sleep_until` panics. A shared async follower in moq-mux was rejected
  because it would move timers into the policy.
- `Control` stores only `target`, `policy`, and `applied` today, so it must
  also remember the pending raise: the last clamped `desired` from `update`.
  Invariant: `deadline()` is `Some(t)` only if `update` with the same
  estimate at `t` will apply a raise. Every `update` that cannot lead to
  one clears the pending raise:
  - an unavailable (`None`) estimate, which holds the target
  - a drop
  - reaching `desired`
  - a `desired` below `policy.max` and within hysteresis of `target`, which
    can never clear the threshold because the ramp is capped at `desired`

  Otherwise `deadline()` is the earliest time the ramp allowance clears the
  hysteresis threshold, or reaches `policy.max` for the exempt final step.
  A due deadline therefore never spins the caller's select arm. Unit-test
  each clearing case, including a scheduled raise followed by a `None`
  estimate.
- Reaching `desired` clears the pending raise only once the caller's
  `set_bitrate` succeeds. Every caller logs and continues on any error
  other than `BitrateUnsupported`, so a transient failure must report back
  to `Control`, which restores the previous target and keeps the pending
  raise. `deadline()` then stays `Some`, no sooner than one ramp step after
  the failure, so a quiet grant still retries and a failing encoder never
  spins the arm. Unit-test it, and cover the quiet-grant path on each caller:
  the final raise fails once, the grant never changes, and the encoder
  still reaches its ceiling.
- The deadline path must not reapply a stale ceiling. moq-ffi's
  `MoqVideoProducer::set_bitrate` releases `inner` before it stores
  `follow.ceiling`, so a deadline that read the old, higher ceiling could
  apply it after the user lowered it. Read the ceiling while holding the
  encoder lock (or store it before releasing `inner`), and test that
  interleaving: a lowered ceiling racing a due deadline never applies the
  old value.
- Fix all three callers in the same PR.

Public API: additive. It adds `moq_mux::rate::Control::deadline`, a way to
report a failed apply, and an async-gated `sleep_until` re-export in
`moq_net::time`. Add a `MIGRATE.md` entry only if an existing signature
changes. Wire: none. Cover the
attack-then-recovery case with paused time in the moq-mux unit tests. Cover
it again on one caller's loop to prove the timer arm wakes without a grant
change.

## Related

- [Simulcast rung disable](/quest/m1/simulcast-rung-disable.md) - also reacts to the bandwidth grant
