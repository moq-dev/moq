# [M] Backport cancellation-safe FFI reads and accepts

## Goal

On the current release line, cancelling a foreign read or accept cannot
consume the next frame or incoming session. Existing synchronous server
cancellation still releases its listener without deadlocking, and runtime
shutdown remains safe. Published signatures and wire behavior stay intact.

## Plan

Decided 2026-10-22 (maintainer): include the paired fixes from
[moq-dev/moq#5140](https://github.com/moq-dev/moq/pull/5140) and
[moq-dev/moq#5172](https://github.com/moq-dev/moq/pull/5172) in the focused
backports. Treat read and accept cancellation together because the server's
synchronous cancellation constrains how its pending accept is driven.

Release 36f392f492 still spawns Task::run work independently. UniFFI's foreign
cancel stops polling before the future is freed, so spawned work can keep
running and take an event during that interval. Reproduce through the actual
UniFFI cancel/free boundary, including delivery between cancellation and
freeing; merely dropping a Rust future does not establish the contract.

Port ownership of pending operations from the two fixes while retaining the
release runtime and listener APIs. Preserve bounded incoming-request storage,
listener release during a parked accept, and synchronization with runtime
shutdown. Do not import main's later worker-pool or transport redesigns.
Cover read/accept cancellation, the next caller's event, server cancellation,
and runtime shutdown with deterministic synchronization. Run affected binding
checks and just test interop --all in addition to the release gate. The
consumers are existing foreign-language readers and native binding servers;
no wrapper migration or new API is intended.

## Related

- [Draining shutdown](/quest/m0/ffi-shutdown-backport.md) - coordinate overlapping FFI files while keeping its additive drain API in its own backport
