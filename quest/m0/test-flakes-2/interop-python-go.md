# [S] Interop's python -> go media cell holds up

## Goal

The `python -> go` media cell of `just test interop --all` passes reliably,
and its quarantine in `test/interop/interop.sh`'s `run_round` is removed.

## Plan

Found 2026-11-10 while iterating
[moq-dev/moq#8444](https://github.com/moq-dev/moq/pull/8444), under the
flaky-test rule. One local `just test interop --all` failed only this cell
and the rerun passed it. On moq-lite-07-wip the Go subscriber (through
moq-ffi) logged `auth granted publish=[] subscribe=["**/<broadcast>"]`.
About 6 s later it logged `publishing outside our grant; closing the session`
from `moq_net::lite::publisher`, and then `audio gap after 0 samples`.

The subscriber holds a publish grant of nothing, so whatever its session
judged to be published, such as a broadcast echoed back through its origin or
a reconnect's re-announce, should never have reached the grant check. Reproduce
it with the interop logs kept, find what the Go client's origin advertises,
fix the cause, then remove the quarantine. A real bug in moq-net's grant
enforcement or in moq-ffi's origin wiring is in scope.

Public API: none expected. Wire: none.
