# [S] The scheduled macOS WebSocket version test completes reliably

## Goal

`moq-tokio`'s `broadcast::broadcast_websocket_uses_newest_version` completes
reliably in the scheduled macOS job, with its client and server assertions
that WebSocket negotiates the newest lite version intact. Fix the timeout at
its cause and cover the failing lifecycle deterministically.

## Plan

Confirmed 2026-11-03 (maintainer, foreground follow-up planning from
[moq-dev/moq#7653](https://github.com/moq-dev/moq/pull/7653)). The maintainer
delegated milestone selection; this belongs in m0 because it blocks reliable
scheduled CI. Cache tuning and unrelated Nightly failures stay outside scope.

- The [November 2 macOS job](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408628726)
  timed out this test after 120 seconds. Reproduce against current `main`
  before changing it, and find which operation stopped progressing.
- The fixture is in `rs/moq-tokio/tests/integration/broadcast.rs`. Its
  connection attempt has a timeout, while its final server-task join waits
  for session closure. That is a place to investigate, not an established
  diagnosis. Distinguish negotiation, transport progress, and teardown.
- Keep the WebSocket transport and both version assertions. Follow the
  [flake epic's test rules](/quest/m0/test-flakes-2/README.md); no larger
  timeout, retry, or weakened assertion substitutes for fixing the cause.
- Validate the affected lifecycle locally and retain the macOS scheduled
  evidence. Do not claim the scheduled failure fixed from Linux-only runs;
  keep any unavailable platform proof explicit.

The job's retained excerpt (timestamps omitted):

```text
TIMEOUT [ 120.015s] (739/739) moq-tokio::integration broadcast::broadcast_websocket_uses_newest_version
running 1 test
test broadcast::broadcast_websocket_uses_newest_version has been running for over 60 seconds
(test timed out)
Summary [ 130.849s] 739 tests run: 738 passed, 1 timed out, 11 skipped
```

The captured test output had no negotiation or teardown diagnostics, so this
evidence does not identify the blocked operation.

Public API: none expected. Wire: none.
