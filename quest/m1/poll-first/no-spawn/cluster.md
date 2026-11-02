# [L] Cluster polls its own work

## Goal

moq-relay's `Cluster` spawns no tasks. `Cluster::new` returns the origin
driver and cache governor as work its owner polls, and `Started::run` drives
every dial, the `api.url` reconciler, and mDNS discovery as `kio::Tasks`
children instead of a tokio `JoinSet` and detached tasks. The relay still exits
with the panic message when a task it cannot serve without panics.

## Plan

Spawns in scope on `main` (2026-11-01), all in `rs/moq-relay/src/cluster.rs`:

- `Cluster::new`: the cache governor and the origin driver, through `Critical`.
- `run_work`: the dial `JoinSet`, the `api.url` reconciler, and mDNS.
- `run_mdns` and `apply_peer_list`: one detached task per peer, tracked by an
  `AbortHandle` in `DialMap`. These become a queue into the `run` loop's
  `Tasks`, so `DialMap` holds `TaskKey`s.
- `Held::drop`: the close of a LAN dial's client. `Dialers` polls its existing
  close set instead.

✅ **`Cluster::new` returns a driver** (maintainer, 2026-11-01, D2 on
[moq-dev/moq#7519](https://github.com/moq-dev/moq/pull/7519)):
`Cluster::new(options) -> anyhow::Result<(Cluster, Driver)>`, the same shape as
`origin::Producer::new`. `Relay::run` polls `Driver` next to `Started::run`.
`Driver` catches each child's panic and turns it into an error, so the relay
still exits with "origin driver panicked: ..." the way `Critical` does now.
Tests spawn the driver, because they are edges. Rejected: keeping the signature
and moving the driver into `Started::run`, which breaks `Cluster::new`'s
documented promise that the origin serves sessions whether or not `start` is
ever called. There are about 68 `Cluster::new(` call sites across moq-relay,
moq-cli, the benches, and moq-net's `mesh_withdraw` test.

✅ **Peer dials stop running in parallel** (maintainer, 2026-11-01, D5 on
[moq-dev/moq#7519](https://github.com/moq-dev/moq/pull/7519)): as `kio::Tasks`
children, every peer dial supervisor runs on the task that polls
`Started::run` instead of its own tokio task. The heavy session I/O is still
spawned inside moq-tokio, which the spawn audit owns, so the cost is probably
small. Measure it once in this PR and report the result.

`Critical` stays for ACME renewal and the web certificate reload, which belong
to the spawn audit; `Cluster::with_stats` keeps spawning the publisher until
[the admissions child](/quest/m1/poll-first/no-spawn/admissions.md) moves it
into `Driver`.

Public API: `Cluster::new` returns `(Cluster, Driver)`; the PR's Impact section
describes the migration. Wire: none.

## Related

- [The remaining spawns](/quest/m2/spawn-audit.md) - owns moq-tokio's session spawns and `Critical`'s remaining tasks
