# Nightly passes on main

## Goal

The scheduled Nightly on `main` passes end to end, so a red run means a new
break instead of noise. Nightly is the backstop for what the PR gate skips on
purpose (benches, examples, feature shapes; see
[moq-dev/moq#5531](https://github.com/moq-dev/moq/pull/5531)), and it has
failed every night since at least 2026-10-19, which let a bench and two
example breaks reach `main` unnoticed.

## Plan

Decided 2026-10-24 (maintainer), from a triage of run
[40090959108](https://github.com/moq-dev/moq/actions/runs/40090959108): one
child per distinct cause, each landing on its own. The PR gate keeps
skipping benches and examples; a green Nightly catches them within a day.

This README's own work, after the children:

- Nightly's multi-step jobs (Features first) keep going past a failed step
  and fail at the end, so one break never hides the steps after it.
- Confirm a scheduled run passes. Failures already owned elsewhere stay
  there: the audio near-zero p95 row belongs to
  [audio jitter target](/quest/m1/audio-jitter-target/README.md), the
  macOS `receive_credit` timeout to
  [test flakes](/quest/m1/test-flakes-2/README.md), and the paired bench
  threshold to [bench CI](/quest/m1/bench-ci.md).

Public API: none. Wire: none.

## Required

- [moq-net compiles in Fuzz and send-depth](/quest/m0/nightly/send-overflow.md) - the ietf publisher future stops overflowing the Send check
- [OBS audio-candidate test](/quest/m0/nightly/obs-audio-candidate.md) - the failed audio candidate check passes under TSan and load, fixed at its reproduced cause
- [moq-net builds without async](/quest/m0/nightly/net-async-free.md) - the no-default-features lane stops pulling `async` back in
- [Feature shapes per crate](/quest/m0/nightly/hls-poll-closed-gate.md) - moq-hls `Broadcaster::poll_closed` is gated on `server`, and the gate checks each crate's feature shapes in isolation
- [moq-relay tests build without default features](/quest/m0/nightly/relay-tests-features.md) - the integration tests gate their websocket and unix-socket fields on the features that define them
- [qmux builds clean for iOS](/quest/m0/nightly/qmux-ios-dead-code.md) - `TcpStats` is gated to the platforms that implement it, so the iOS simulator build has no dead code
- [Probe against Cubic](/quest/m0/nightly/competing-cubic.md) - the `competing_cubic` probe test stops resending a stream after its reset
