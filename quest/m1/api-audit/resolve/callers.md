# [M] Callers resolve once

## Goal

No caller hand-rolls synced resolution or relies on a wall clock to decide a
broadcast is absent. Each first-party caller resolves with `request` or
`follow`.

## Plan

Split from [Two broadcast resolutions](/quest/m1/api-audit/resolve/README.md)
on 2026-10-28 (maintainer).

- Remove the racy two-step lookup and its hidden error in moq-wasm.
- Replace the route waits in moq-cli's subscribe and report.
- Review SRT, transcode, the benchmark connections, relay HTTP fetch
  deadlines, the e2ee epoch advice, and the demo's announce-derived epoch pin;
  fix each that hand-rolls resolution or decides absence by time.

List every caller in the PR with what it now uses. Run the affected packages'
tests and `just test interop --all`.

Public API: none beyond the callers' own surfaces. Wire: none.

## Required

- [Rust request and follow](/quest/m1/api-audit/resolve/rust.md) - the Rust names the callers move to
