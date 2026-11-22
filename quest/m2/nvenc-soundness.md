# [S] Fix moq-nvenc Send/CUDA/session soundness and null-pointer UB

## Goal

`moq-nvenc` removes unsound `unsafe impl Send` impls, gates raw CUDA resource
registration behind `unsafe` (or sound encapsulation), prevents concurrent/post-
EOS encoder misuse on `Session`, and eliminates null-pointer UB in bitstream
locking and error string formatting.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fix the unsound public APIs in all three vendored
native crates (`moq-v4l`, `moq-vaapi`, `moq-nvenc`) in `m2` before `0.22.0`
publishes them (rejected: non-breaking internals only in `m1` and deferring
breaking API fixes to `m3`; skipping and tracking upstream only).

1. **Remove unsound `unsafe impl Send` (`rs/moq-nvenc/src/safe/buffer.rs:305,529,684`).**
   - `Buffer`, `Bitstream`, and `RegisteredResource<MappedBuffer>` implement
     `Send` despite holding `!Send` NVENC session/context pointers or non-`Sync`
     references; remove the unsound `unsafe impl Send` impls (or bound them
     strictly on proven thread-safety invariants).
2. **Make CUDA resource registration sound (`rs/moq-nvenc/src/safe/buffer.rs:235-252`).**
   - Make `Session::register_cuda_resource` an `unsafe fn` (or encapsulate
     `MappedBuffer` ownership/lifetimes so callers cannot pass arbitrary device
     pointers or drop the backing allocation early) and preserve the CUDA sync
     guard across map/unmap.
3. **Fix `Session::encode_picture` / `end_of_stream` API (`rs/moq-nvenc/src/safe/session.rs:172-175,237-280`).**
   - Require `&mut self` (or enforce single-flight state), return the input
     buffer/bitstream on `NV_ENC_ERR_ENCODER_BUSY` instead of losing/leaking
     ownership, and reject encoding calls after `end_of_stream`.
4. **Fix null-pointer UB:**
   - `rs/moq-nvenc/src/safe/buffer.rs:581-583` (`Bitstream::lock_inner`): check
     for null pointer and zero length before constructing a slice, using
     `std::slice::from_raw_parts` (never passing null to `from_raw_parts`).
   - `rs/moq-nvenc/src/result.rs:254` (`NVENCSTATUS::result`): check the
     returned C string pointer for null before `CStr::from_ptr`.

Update in-tree callers (`rs/moq-video`) for the updated `Session`/`Buffer` APIs.

Public API: breaking in `moq-nvenc` (`Send` removal, `register_cuda_resource`
safety, `encode_picture`/`end_of_stream` signatures). Wire: none. Add unit
tests for `NVENCSTATUS` null handling, empty bitstream lock handling, and
compile-fail (`trybuild` or doc-test `compile_fail`) checks for `!Send` and
safe `register_cuda_resource`.
