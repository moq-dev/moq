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
  threshold to [bench CI](/quest/m1/bench-ci.md). The `cli-export`
  bufferbloat failure is expected to clear with
  [moq-dev/moq#6656](https://github.com/moq-dev/moq/pull/6656).

Public API: none. Wire: none.

## Required

- [Nightly's base in a shallow checkout](/quest/m0/nightly/shallow-base.md) - `sh/gh/nightly.test.py` passes in Nightly's depth-1 checkout
- [Windows builds moq-relay tests](/quest/m0/nightly/windows-relay-expect.md) - an unfulfilled lint expectation stops failing the Windows build
- [OBS audio-candidate test](/quest/m0/nightly/obs-audio-candidate.md) - the TSan test waits for pending_audio to clear instead of racing it
- [moq-hls without default features](/quest/m0/nightly/hls-poll-closed.md) - `Broadcaster::poll_closed` is gated with its only caller, so the no-default-features build stops warning
