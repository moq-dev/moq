# [XS] The relay bench test passes local checks on macOS

## Goal

`just check` passes on macOS: `bench/relay.test.py`'s `test_sampler_stops_generator_after_collecting_both_processes` runs only where the branch it drives runs.

## Plan

Found while validating [#7781](https://github.com/moq-dev/moq/pull/7781) on 2026-11-03; it fails the same way on `main`.
The test drives `bench/relay.sh`'s Linux-only sampler branch, where the generator runs until interrupted.
On macOS, `run_workload` takes the other branch, which passes `--duration 10s`, and the test's fake generator fails `assert '--duration' not in rest`.
Linux CI is unaffected.

Restrict the test to Linux the way its sibling is (`skipUnless(sys.platform == "linux")` at `bench/relay.test.py:111`).
If no test covers the non-Linux branch, add a small one there rather than leave it unexercised.
