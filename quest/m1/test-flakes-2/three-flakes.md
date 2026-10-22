# [XS] OBS drain and JS allocation flakes

## Goal

Each passes reliably under a loaded `just check`, fixed at its cause:

- OBS "stop drains the session" (`cpp/obs/test/moq-output-test.cpp`, a
  16 MiB frame under TSan), seen in
  [moq-dev/moq#6131](https://github.com/moq-dev/moq/pull/6131) and
  [moq-dev/moq#6137](https://github.com/moq-dev/moq/pull/6137).
- The js/net RSS memory test, which flakes under load
  ([moq-dev/moq#6128](https://github.com/moq-dev/moq/pull/6128)).

## Plan

Reproduce before fixing, with no retries, sleeps, or raised timeouts.
The unchanged OBS suite passed both a full TSan run and an output-only TSan
run pinned to one CPU on 2026-10-21. Its artificial 16 MiB frame competes
with the session's intentional one-second drain deadline, but a local
failure has not yet established the right regression. Keep the lifecycle
assertions when separating that contract from throughput.

[moq-dev/moq#6188](https://github.com/moq-dev/moq/pull/6188) owns the RSS
replacement: bounded algorithm assertions rather than process RSS. Do not
duplicate it; verify its merged result under load.

The original audio-quality RTT and fresh-worktree interop conditions are
implemented: the sampler observes PROBE while sampling, and the interop
entry point installs Bun dependencies before Rust invokes the JS codecs.

Public API: none. Wire: none.
