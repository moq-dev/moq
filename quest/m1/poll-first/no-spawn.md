# [L] Libraries spawn nothing

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

Per crate: moq-ffi spawns only its root at the binding boundary; moq-stats and moq-relay's `Cluster::new` and auth decider
return work their owner polls. List any spawn that has to stay, with why, in
the PR.

Public API: constructors that spawned return a handle to poll or await
instead. The PR's Impact section describes migration for the published crates whose API changes. Wire:
none.

## Related

- [The remaining spawns](/quest/m2/spawn-audit.md) - the same rule for every other library crate
- [Generated async wrappers](/quest/m1/poll-first/async-feature.md) - the poll-first sibling that gates the async layer
