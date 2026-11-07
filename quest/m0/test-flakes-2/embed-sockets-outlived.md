# [S] The embedded relay's sockets are closed when run returns

## Goal

moq-relay's `embed::worker_tokio_custom_route_and_quic`
(`rs/moq-relay/tests/integration/embed.rs`) passes reliably under load,
fixed at its cause under the epic's rules, and its `#[ignore]` and
`sh/gh/test-inventory.allow` entry are removed. Either `run` returns only
after every worker's socket is closed, or the test's check stops racing a
close that `run` already guarantees.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#8138](https://github.com/moq-dev/moq/pull/8138)'s optional
Check ([run](https://github.com/moq-dev/moq/actions/runs/42352819862)). That PR
touches only `sh/` and quest docs, yet the test panicked at `embed.rs:351`
with "the owner's sockets outlived `run`". Following AGENTS.md's rule to
disable a flaky test at once, the test is `#[ignore]`d until this quest fixes it.

[moq-dev/moq#7823](https://github.com/moq-dev/moq/pull/7823) already moved
Linux to the socket-inode ownership proof (`bound` before `stop`, then
`open_sockets` after), so a reused port cannot explain this failure. A failure
there means an inode bound before `stop` was still in the process's fd table
after `run` returned. With `runtime.workers = 2`, the thread-per-core Tokio
workers each bind their own `SO_REUSEPORT` socket. Suspect a worker, or a
socket it owns, that drops after `run` joins it, such as a spawned task or a
connection that outlives the join.

Reproduce it first, for example with repeated runs of this one test beside a
CPU-heavy job. Record which inode outlived `run` and who owns it. Then make
`run` close it before returning, or show that the close is already ordered
before `run` returns and fix the check instead. No retry, sleep, or settle
window.

The assertion lives in the shared `embed_and_stop` helper, so
`shared_tokio_custom_route_and_quic` (default CI) and
`uring_custom_route_and_quic` (nightly's `just rs uring`) assert the same close.
Verify the fix on all three before removing the `#[ignore]`. If
the cause is the check racing a close rather than a leak in the worker mode,
the fix covers the other two as well.
