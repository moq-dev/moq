# [M] Integrate the hardened reuseport group with QUIC workers

## Goal

The owning tokio worker group uses `moq_sock::shard::Group`'s enforced
formation and socket-retention contract. Complete and retained socket groups
preserve connection-ID steering through partial startup and member teardown.

## Plan

The published `Workers::split` ownership change and functional group shutdown
have landed. Do not repeat
that API redesign here. This follow-up replaces the worker's private lifetime
bookkeeping with the hardened socket-group primitive after dev merges.

Adopt moq-sock's claims and complete-group ownership. No member serves before
the final bind and filter attachment, and every socket stays owned until
serving has stopped for the group. Preserve the dev owner, shutdown, failure
propagation, and worker-local builder contract without another published
signature change.

Use Linux runtime regressions to exercise incomplete startup, a failed final
bind, dropping an unused server handle, and a serving member completing or
failing. Check the socket group and connection-ID steering on a surviving
session while unused handles are dropped, and prove all serving stops when
the group terminates. Wire the tests into normal or nightly CI.

Public API: no further `moq-tokio` ownership change. The prerequisite may
change `moq-sock`'s 0.0.x API. Wire: no format change. Close #2964 only when
both the dev ownership proof and this integration are complete.

## Closes

- [#2964](https://github.com/moq-dev/moq/issues/2964) - close this issue when the quest finishes
