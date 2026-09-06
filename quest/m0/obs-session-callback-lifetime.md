# [M] Keep OBS output alive until session callbacks finish

## Goal

Destroying a MoQ output must not free state that a pending libmoq session callback can access, even when the runtime is stalled longer than two seconds.

## Plan

`MoQOutput::~MoQOutput` in `cpp/obs/src/moq-output.cpp` waits at most two seconds for `outstanding_sessions`, logs a warning, then frees the object. `SessionRef` retains a raw output pointer until its terminal callback calls `SessionClosed`. A delayed terminal can therefore dereference freed state. This predates the dock stats work in PR #3453.

- Move session callback state into an independently owned lifetime or establish a teardown barrier that cannot expire while callbacks still reference the output. Keep OBS calls and lock ordering explicit.
- Test a terminal held beyond the old timeout, then released after teardown begins. Use deterministic synchronization and ASan/TSan to verify no stale output access.
- Cover normal shutdown, failed startup, superseded attempts, and frontend re-entry without blocking the GUI on queued work.
