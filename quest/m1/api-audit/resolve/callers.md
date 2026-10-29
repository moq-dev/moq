# [M] Callers resolve once

## Goal

No caller hand-rolls synced resolution or relies on a wall clock to decide a
broadcast is absent. Each first-party caller resolves with `request` or
`follow`.

## Plan

Split from [Two broadcast resolutions](/quest/m1/api-audit/resolve/README.md)
on 2026-10-28 (maintainer).

- Remove the hidden error in moq-wasm's resolve, which follows the path but
  still discards why it failed.
- Replace moq-cli's resolve-and-drop waits before its stdout, RTMP, and WHIP
  exports, which follow the path only to wait for its announcement and then
  resolve it again.
- Review SRT, transcode, the benchmark connections, relay HTTP fetch
  deadlines, the moq-hls server's `RESOLVE_TIMEOUT` handler-stall bound, the
  e2ee epoch advice, and the demo's announce-derived epoch pin; fix each that
  hand-rolls resolution or decides absence by time.

List every caller in the PR with what it now uses. Run the affected packages'
tests and `just test interop --all`.

Public API: none beyond the callers' own surfaces. Wire: none.
