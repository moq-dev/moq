# [M] moq-ffi spawns only its root

## Goal

moq-ffi spawns one task outside its UniFFI calls: a runtime-owned driver that
polls every `Background` as a `kio::Tasks` child. Origin drivers, the server
accept loop, cleanups, and demand waits no longer spawn tasks of their own.

## Plan

Spawns in scope on `main` (2026-11-01), all in `rs/moq-ffi/src/`:

- `ffi.rs`, `ffi::spawn` and `Background::driver`: origin drivers
  (`origin.rs`), the server accept loop (`server.rs`), and cleanups.
- `ffi.rs`, `detached`: demand waits (`demand.rs`, `producer.rs`).
- `ffi.rs`, each UniFFI call's task on the bindings' runtime. This stays: each
  call's own future is the binding boundary.

✅ **One root driver** (maintainer, 2026-11-01, D4 on
[moq-dev/moq#7519](https://github.com/moq-dev/moq/pull/7519)): moq-ffi's root
is one runtime-owned driver task that holds every `Background` as `kio::Tasks`
children. Rejected: having each handle's calls poll its `Background`, which
fails for an origin with no reader, since nobody would poll its driver.

[moq-dev/moq#7497](https://github.com/moq-dev/moq/pull/7497) replaced
`ffi::Task` with `ffi::Polled` for kio-backed reads and changed `Background`
and shutdown in `ffi.rs`, keeping demand waits `detached`. Recount the sites
above against it before converting.

The runtime-owned root and in-place call waits are implemented in
[moq-dev/moq#7720](https://github.com/moq-dev/moq/pull/7720). Completion still
requires separating foreign continuation dispatch from root polling, or an
explicit decision to accept the resulting process-wide stalls. UniFFI wakes
foreign code inline; one blocking accept continuation now holds the root and
prevents another server from accepting. Keep the original two-callback JVM exit
probe as evidence while this is resolved.

Public API: no binding signature changes. Wire: none.

## Waiting

- Maintainer: choose how foreign continuations coexist with the single root.
  Options: dispatch foreign wakes outside root polling through an additional
  runtime dispatch mechanism; require every binding to dispatch continuations;
  or accept and document that a blocking continuation stalls every background
  child. Recommend dispatching foreign wakes outside the root, preserving
  callback independence while keeping every `Background` under one owner.
  This may refine the one-runtime-task boundary. Check: settle the choice on
  [moq-dev/moq#7720](https://github.com/moq-dev/moq/pull/7720), then implement it
  and pass the unchanged JVM exit probe.
