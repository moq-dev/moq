# [M] Audio encodes on demand from a raw backbuffer

## Goal

With no subscriber, the audio publishers in JS and Rust keep captured raw
audio up to the track's `maxAge` instead of encoding it. When demand arrives,
they encode that backlog and continue live, so a late subscriber can still
fetch the recent past within its `maxDelay`. This becomes audio's default.
Video stays always-on.

## Plan

Implementation is in [PR #7238](https://github.com/moq-dev/moq/pull/7238).
The remaining work is validation and review, not an unsettled implementation choice:

- Get a complete `just ci` result. The local run could not spawn Cargo through
  `mbx` (`Argument list too long`) in the Rustdoc, WASM, and lint-enforcement
  lanes. The audio suites, CLI policy test, package build, API report, and
  Chromium and Rust benchmark sweeps pass independently.
- Resolve findings, then delete this quest and its links and mark the PR ready.

Implemented choices, under the maintainer's 2026-10-28 decision and resumed
unattended authorization:

- Audio keeps capture open and buffers codec-sized PCM up to `maxAge`. Without
  a declared retention, it keeps 30 seconds, matching hang's media default:
  about 11.5 MB for 48 kHz stereo f32, plus a partial frame and codec state.
  Time and sample-count bounds prevent repeated timestamps growing the buffer.
- Demand replays retained audio in capture order before live frames. Losing
  demand resumes buffering without cutting the epoch. Codec framing,
  timestamps, and configured `groupDuration` match live encoding; the older
  one-group-per-frame wording predated configurable grouping.
- JS's omitted `Broadcast.onDemand` chooses buffered audio and always-on video.
  Explicit false encodes both continuously; true also gates video. Rust's
  `encode::Capture::on_demand` defaults to true and leaves the microphone open.
  The CLI follows the same defaults; `--on-demand=false` opts out.
- Rust yields between codec frames during catch-up so the capture queue can
  keep draining. Bandwidth reservations still depend on subscribers.

Public API: audio's default behavior and Rust capture lifecycle change; JS's
`BroadcastInput.onDemand` can be undefined and CLI `--on-demand` accepts an
optional boolean. Set false to retain continuous audio encoding. Wire: none.
