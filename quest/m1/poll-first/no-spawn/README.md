# Libraries spawn nothing

## Goal

moq-ffi, moq-stats, and moq-relay's cluster and auth decider spawn no tasks:
their background work runs as `kio::Tasks` children that the owner polls,
returned as a handle whose poll drives it. Only the edges spawn a root task:
binaries and the bindings' runtime. moq-relay's in-memory integration tests
(`rs/moq-relay/tests/integration/connection.rs`) run on kio's simulator
instead of paused tokio.

## Plan

Decided with the maintainer on 2026-10-25, replacing the m2 runtime trait
(spawn and time for tokio, browser, io_uring, and sim):

- **Poll everywhere, no spawn.** A spawn trait keeps `'static` detached tasks
  in the library, which poll-first rejects; `kio::Tasks` already gives a
  library structured children with no runtime. Time is
  `kio::clock`, so nothing is left for a runtime trait.
  `rs/AGENTS.md` already treats `tokio::spawn` in a library as a smell.
- **No I/O seam.** I/O stays at moq-net's `transport::poll` traits and
  moq-sock: tokio's readiness model and io_uring's completion model (the
  kernel owns the buffer) can't share a zero-cost API.
- **Blocking work is out of scope.** OS threads for blocking capture, encode,
  or file I/O are not async tasks and stay.

✅ **Split into four children** (maintainer, 2026-11-01, D1 on
[moq-dev/moq#7519](https://github.com/moq-dev/moq/pull/7519)), landed in the
order of Required below. One PR could not hold them all, and two open PRs
already changed part of the scope:
[moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360) (merged
2026-11-01) removed the auth decider loop and returns moq-stats' publisher from
`Producer::new`, and
[moq-dev/moq#7497](https://github.com/moq-dev/moq/pull/7497) rewrites moq-ffi's
`Task` and `Background` plumbing. Each child records its own decisions.

Every child lists any spawn that has to stay, with why, in its PR. Public API:
constructors that spawned return a handle to poll or await instead. Each PR's
Impact section describes migration for the published crates whose API changes.
Wire: none.

These spawns stay and belong to [the spawn audit](/quest/m2/spawn-audit.md):
`Critical`'s ACME renewal (`rs/moq-relay/src/relay.rs`) and web certificate
reload (`rs/moq-relay/src/web.rs`), the io_uring handoff in
`rs/moq-relay/src/connection.rs`, and moq-relay's web, shutdown, and uring
spawns. Each UniFFI call's task on the bindings' runtime stays too: that is the
binding boundary.

## Required

- [Cluster polls its own work](/quest/m1/poll-first/no-spawn/cluster.md) - `Cluster::new` returns a `Driver` the relay polls, and the dial loop runs as `kio::Tasks` children
- [Admissions and stats are polled](/quest/m1/poll-first/no-spawn/admissions.md) - HTTP admissions run inside their waiter, and the stats publisher runs in the cluster's driver
- [moq-ffi spawns only its root](/quest/m1/poll-first/no-spawn/ffi.md) - one runtime-owned driver task holds every `Background` as `kio::Tasks` children
- [The relay's in-memory tests run on kio's simulator](/quest/m1/poll-first/no-spawn/sim-tests.md) - `rs/moq-relay/tests/integration/connection.rs` runs on the simulator instead of paused tokio

## Related

- [The remaining spawns](/quest/m2/spawn-audit.md) - the same rule for every other library crate
- [Generated async wrappers](/quest/m1/poll-first/async-feature.md) - the poll-first sibling that gates the async layer
- [Poll-native async in moq-ffi](/quest/m2/ffi-poll-migrate.md) - its PR reworks the moq-ffi task plumbing this epic touches
