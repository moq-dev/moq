# [M] Fix moq-vaapi Image soundness, NV12 upload readback/OOB, and FFI bounds

## Goal

`moq-vaapi`'s public `Image` API prevents aliased `&mut [u8]` borrows and
unmaps before `vaPutImage`, NV12 upload avoids per-frame GPU→CPU readbacks and
last-row OOB panics, and VA-API FFI wrappers validate fds, buffer pointers,
lengths, GOP, and H.264/H.265 decode bounds.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fix the unsound public APIs in all three vendored
native crates (`moq-v4l`, `moq-vaapi`, `moq-nvenc`) in `m2` before `0.22.0`
publishes them (rejected: non-breaking internals only in `m1` and deferring
breaking API fixes to `m3`; skipping and tracking upstream only).

1. **Breaking API soundness in `rs/moq-vaapi/src/image.rs:58-61,95-98,199-229`.**
   - `Image::derive_from` takes `&'a Surface<D>` (shared ref) while `Image`
     exposes mutable slice access, letting safe code derive two `Image`s from
     one `Surface` and create aliasing `&mut [u8]` references (immediate UB).
     Require `&'a mut Surface<D>` (or split read-only and mutable image types).
   - `Image::drop` calls `vaPutImage` while the image buffer is still mapped,
     then calls `vaUnmapBuffer` afterwards; reorder so `vaUnmapBuffer` runs
     before `vaPutImage` (and avoid panicking in `Drop`).
   - Replace the post-map `assert!` in `Image::new` (`:58-61`) with validated
     `Result` errors using an RAII guard so failed validation still unmaps and
     destroys the image cleanly.
2. **NV12 upload performance and OOB fix (`rs/moq-vaapi/src/encode.rs:544,773-814`).**
   - `upload_nv12` currently calls `Image::new` (which runs `vaGetImage`, a
     full GPU→CPU readback) on every frame and looks up the NV12 `VAImageFormat`
     repeatedly. Add a write-only `Image::create_uninitialized` (skips
     `vaGetImage`) and cache the NV12 format on the encoder.
   - Fix the last-row slice advance `dst = &mut dst[pitch..]` when the final
     plane row has fewer than `pitch` trailing bytes (prevents panic on tight
     buffers).
   - Reject `config.gop == 0` at `encode.rs:544` with an explicit error instead
     of dividing/modding by zero.
3. **FFI pointer, fd, length, and decoder bounds validation:**
   - Validate `obj.fd >= 0` before wrapping in `OwnedFd` (`rs/moq-vaapi/src/surface.rs:364`).
   - Guard null `segment.buf` and zero-length slices in `rs/moq-vaapi/src/buffer.rs:378`.
   - Validate `max_num` / `num` against vector capacity before `Vec::set_len`
     in `rs/moq-vaapi/src/display.rs:178-191,220-239,354-368`.
   - Cap the H.264 reference gap-fill loop to `max_num_ref_frames`
     (`rs/moq-vaapi/src/decode.rs:738-799`) and use checked/wrapping POC
     arithmetic (`decode.rs:697-711`, `rs/moq-vaapi/src/decode/h265.rs:395`).

Update in-tree callers (`rs/moq-video`) for the updated `Image` signature.

Public API: breaking in `moq-vaapi` (`Image::derive_from` mutability /
read-only split, `Image::create_uninitialized`). Wire: none. Add unit tests for
bounds validation, `gop == 0`, tight NV12 plane slicing, and H.264/H.265 POC
wraparound; run VA-API tests where hardware is available.
