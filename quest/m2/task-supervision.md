# [S] The remaining long-lived tasks are supervised

## Goal

A panic in the relay's auth decider loop, the stats publisher, or
moq-tokio's token and WebSocket internals either ends only what depends on
it or exits the process, by the rule [#5447](https://github.com/moq-dev/moq/pull/5447)
set: a per-connection task's panic is isolated and counted, a long-lived
task's panic propagates out of `Relay::run` or the CLI's main. None is
swallowed.

## Plan

Follow-up from #5447, which supervised the session drivers and the relay's
critical tasks but left these. Decided 2026-10-13 (maintainer): m2, low
priority, since each is a single process-wide task. Classify each task as
per-connection or long-lived, observe its `JoinHandle` with
`moq_tokio::panic`, and add a regression that injects a panic into one task
of each class.

Folded in from #5447's review (maintainer, 2026-10-13): `Server::serve_*`
still returns `Ok(())` when a transport's accept loop dies, because
`Listener::accept` only yields requests. Surface the dead transport's error
through `Listener::accept`, so a server built on it fails loud the way the
relay and the CLI already do.

Public API: `Listener::accept`'s signature in moq-tokio (and
`Server::serve_*`'s result). Wire: none.
