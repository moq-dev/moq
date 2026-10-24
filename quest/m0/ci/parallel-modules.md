# [M] Run the queue's modules in parallel

## Goal

`just ci` runs its selected modules concurrently instead of one after another,
with the same modules selected and the same failure behavior, and a
multi-language entry finishes measurably faster.

## Plan

Decided with the maintainer on 2026-10-23. Measured 2026-10-23: `sh/dispatch.sh`
runs every selected module serially. On multi-language entries (863 to 876 s),
the cpp, dart, py, kt, and obs modules take about 400 to 500 s that could
overlap the Rust build, an estimated 45% off those entries.

- Bound concurrency so modules do not starve each other, and keep each
  module's output readable when they interleave.
- A failure still fails the run and names its module, and cancels the
  modules still running, so a cheap check that fails ends the entry in its own
  time. In [queue run 39996587979](https://github.com/moq-dev/moq/actions/runs/39996587979)
  the tooling tests failed after 18 s, but only after about 16 minutes of
  modules ahead of them. Verify an injected `just gh test` failure ends the run
  before the Rust build finishes, and that cancellation leaves no module
  running. Decided 2026-10-23: this replaces a separate quest to move cheap
  checks ahead of builds in the serial loop.

Public API: none. Wire: none.

## Related

- [Share local check capacity](/quest/m0/ci/local-gate.md) - the same fanout bound applies locally
