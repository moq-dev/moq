# [M] The relay's in-memory tests run on kio's simulator

## Goal

moq-relay's in-memory integration tests
(`rs/moq-relay/tests/integration/connection.rs`) run on kio's simulator
instead of paused tokio.

## Plan

This needs the cluster's and the admissions' work to be polled, which the
children it requires deliver. It also needs `moq_tokio::origin::spawn` and the
memory connect helper the tests use to run without tokio. moq-tokio belongs to
[the spawn audit](/quest/m2/spawn-audit.md) (m2), so this quest may move to
m2, or require the part of the spawn audit that converts those two helpers.
Decide that when it starts.

Public API: none. Wire: none.
