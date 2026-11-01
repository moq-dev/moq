# [S] Admissions and stats are polled

## Goal

moq-relay spawns no task per HTTP admission, and `Cluster::with_stats` spawns
no stats publisher. The `Server` decider's request runs inside the caller that
waits on its answer, and the publisher runs as a child of the cluster's
`Driver`.

## Plan

Spawns in scope on `main` (2026-11-01):

- `rs/moq-relay/src/auth.rs`, `Decider::Server`: one
  `tokio::spawn(moq_tokio::panic::isolate(..))` per admission HTTP request.
  [moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360) (merged
  2026-11-01) already removed the decider loop around it.
- `rs/moq-relay/src/cluster.rs`, `Cluster::with_stats`: the stats publisher,
  through `Critical`. Since #7360, `moq_stats::Producer::new` returns
  `(Producer, Publisher)`, so moq-stats itself spawns nothing.

✅ **The waiter polls the admission** (maintainer, 2026-11-01, D3 on
[moq-dev/moq#7519](https://github.com/moq-dev/moq/pull/7519)): the caller
waiting on `Answer` polls the `moq_auth::Client` request inline, and dropping
the wait cancels the request, which replaces the `reply.closed()` select. A
panic then ends the session being admitted, the same scope the per-admission
`isolate` gives today. Rejected: one long-lived `kio::Tasks` of admissions
polled by `Relay::run`, which brings back the long-lived decider task that
#7360 removed.

The publisher joins `Driver`, the type
[Cluster polls its own work](/quest/m1/poll-first/no-spawn/cluster.md)
introduces, so a publisher panic still ends the relay. `Publisher::run` is an
`async fn`; giving `Publisher` a `poll` is optional and only worth it if
`Driver` needs it.

Public API: `Cluster::with_stats` no longer needs a tokio runtime. Wire: none.

## Required

- [Cluster polls its own work](/quest/m1/poll-first/no-spawn/cluster.md) - provides the `Driver` the publisher joins
