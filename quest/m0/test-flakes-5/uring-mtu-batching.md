# [S] moq-uring's MTU discovery test keeps its batching assertion under load

## Goal

`moq-uring`'s `mtu::discovery_finds_a_larger_mtu` passes under a loaded merge queue, fixed at its cause.

## Plan

Seen in the merge queue on 2026-11-27, where it knocked a PR out:
`moq-uring::integration mtu::discovery_finds_a_larger_mtu` panicked at
`rs/moq-uring/tests/integration/mtu.rs:333`, which is the final check, `datagrams > 2 * sends`.
So discovery and the large-datagram share passed, and only send batching fell short.

First decide whether a loaded host legitimately sends fewer datagrams per send, for example because
the worker flushes before a train fills. If so, assert on what the test owns, such as GSO segments
per send while a train is queued, rather than a ratio that depends on scheduling. If not, fix the
batching. Reproduce under load first. Plain CPU load did not reproduce the finish-row race in
[moq-dev/moq#9810](https://github.com/moq-dev/moq/pull/9810); randomly pausing the process did.

Public API: none. Wire: none.
