# [S] OBS TSan passes without a wall-clock bound

## Goal

Nightly's OBS TSan job passes, and `destroySource` in
`cpp/obs/test/moq-source-test.cpp` no longer asserts elapsed wall time.

## Plan

The deterministic source teardown assertions are implemented. Production code is
unchanged: the test observes cancellation of the connect continuation, releases
a running worker task only after cancellation returns, then checks destruction
waited for that task. Shared teardown checks connection release, executor closure,
and allocation balance. The missing-broadcast scenario waits for its pending call.

Local validation reproduced the old elapsed-time failure under TSan with a delayed
worker, and the updated source suite passes under TSan. The unmodified full OBS
TSan suite also passes locally. Formatting and quest checks pass; local self-review
has no findings.

Finish validation before marking the PR ready:

- Run `just ci` and the plain OBS unit suite (`bash sh/obs/unit.sh`) through
  `quest gate`.
- Confirm the controlled connect test rejects joining the worker before cancelling
  the connection, and that delayed but correctly ordered teardown passes.

These remaining commands stayed queued at host admission, so the attempt stopped
without bypassing `quest gate`. Delete this quest and its links once validation
passes. Do not merge, enqueue, or wait for online checks or reviews in this attempt.

Public API: none. Wire: none.
