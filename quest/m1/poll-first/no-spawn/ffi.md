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

Public API: no binding signature changes. Wire: none.
