# [M] Fix moq-ffi async runtime routing, window finish retention, libmoq enum exports, and timestamp guards

## Goal

Every exported `moq-ffi` async method runs inside `ffi::run` with panic
isolation and stop handling, `MoqJsonWindowProducer::finish` retains its
finished handle, `MoqServer::listen` cannot strand `Phase::Listening` on
cancellation, `libmoq` exports `moq_order` and `moq_refresh` in `moq.h`, and
Kotlin/Python/Dart timestamp conversions guard zero/negative/infinite inputs.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful correctness/FFI bugs
in `m1` (rejected: `m0` with `release` backports; `m3`).

1. **`rs/moq-ffi/src/ffi.rs:547-564` & `rs/moq-ffi/src/media.rs:1902-1911`**:
   - Route `Background::watch` (10 exported async methods) and
     `MoqMediaCatalogConsumer::subscribe` through `ffi::run` so they get
     `Polling`, `stopped()`, `ffi::enter()`, and `panic::isolate` consistently
     with every other exported async entry point.
2. **`rs/moq-ffi/src/json.rs:638-644`**:
   - Retain `moq_json::window::Finished` in `Children` when
     `MoqJsonWindowProducer::finish()` runs so finishing a window producer does
     not immediately drop the finished track state.
3. **`rs/moq-ffi/src/server.rs:255,293-336`**:
   - Introduce `Phase::Starting` plus a `Drop` guard in `MoqServer::listen` so
     cancelling an in-flight listen call rolls back phase cleanly instead of
     stranding the server in `Phase::Listening`.
4. **`rs/libmoq/build.rs:8-22`**:
   - Add `"moq_order"` and `"moq_refresh"` to `ENUMS` so cbindgen exports both
     enums in `moq.h`.
5. **Binding timestamp safety:**
   - `kt/moq/.../Timestamp.kt:24-45`: fix negative wrap in `toDuration()`,
     guard `scale == 0`, and reject `Duration.INFINITE` / >292-year values in
     `toTimestamp()` instead of silently saturating.
   - `py/moq-rs/moq/_records.py:86-94`: guard `scale == 0` with `ValueError`.
   - `dart/moq/lib/src/timestamp.dart:15-27`: guard `scale <= 0` with
     `ArgumentError`.

Public API: additive header export in `libmoq` (`moq_order`, `moq_refresh`);
no breaking API changes. Wire: none. Add unit tests across `rs/moq-ffi`,
`rs/libmoq`, Kotlin, Python, and Dart.
