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
[moq-dev/moq#7720](https://github.com/moq-dev/moq/pull/7720). UniFFI wakes
foreign code inline, so a blocking host continuation woken by a child held the
root and every other child.

✅ Decided 2026-11-03 (maintainer, /quest-unblock): dispatch foreign wakes
outside root polling. A runtime dispatch mechanism keeps a host continuation
that resumes inline from ever running inside the root's poll of a child, so a
blocking callback cannot stall the other background children, and every
`Background` stays under the one root owner. Not chosen: requiring every
binding to dispatch continuations; accepting and documenting process-wide
stalls. Check: the unchanged two-callback JVM exit probe passes.

This refines the "one runtime task" boundary of D4 on
[moq-dev/moq#7519](https://github.com/moq-dev/moq/pull/7519): the root stays
the only task that owns or polls background work. A wake the root raises for a
parked host call is handed to the runtime's blocking pool, where a short-lived
task only invokes the host's waker (coalesced per call, one per continuation in
flight). It owns no state and polls no `Background`. Wakes from anywhere else
still reach the host inline.

Public API: no binding signature changes. Wire: none.
