# [S] Admissions and stats are polled

## Goal

moq-relay spawns no task per HTTP admission, and the stats publisher runs as a
child of the cluster's `Driver`. The caller waiting on an admission answer
owns and polls its HTTP request, so dropping the wait cancels the exchange.

## Plan

[moq-dev/moq#7673](https://github.com/moq-dev/moq/pull/7673) implements the
caller-owned admission future and queues stats publishers for the existing
cluster driver. The initial implementation passed `just ci`, including the
runtime-free setup, HTTP cancellation, and stats driver-lifetime tests.

The caller-owned admission request is settled by the maintainer's D3 on
[moq-dev/moq#7519](https://github.com/moq-dev/moq/pull/7519). Keep it; do not
reintroduce a long-lived decider supervisor. In-band verification slots and
end-event ordering remain unchanged.

The first local review found a construction tradeoff: the queue keeps
`Cluster::with_stats`, but adds two locks to each driver poll and panics if
stats are attached after its driver drops. Moving stats configuration into
`cluster::Options` lets `Cluster::new` build it against its origin and add the
publisher to the driver directly, removing the handoff entirely. That changes
the public construction API, so it needs the maintainer's pick.

The review also found that the HTTP/1.1 admission benchmark could exhaust a
1024-fd process limit and wait forever during setup. Its fix keeps the server
sweep within that budget and fails when a held admission completes early.

Decided 2026-11-03 (maintainer, /quest-unblock): ✅ **Move cluster stats
configuration into `cluster::Options` and remove `Cluster::with_stats`.**
Construction adds the stats publisher directly to the driver, removing the
handoff queue, the two locks per driver poll, and the panic after the driver
drops. Not chosen: keep `with_stats` and its queue. Update every in-tree
caller (moq-relay) and write migration notes in the PR's Impact section and
`rs/moq-relay/MIGRATE.md`.
