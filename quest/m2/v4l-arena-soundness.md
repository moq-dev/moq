# [S] Fix moq-v4l Arena soundness and stream/device safety bugs

## Goal

`moq-v4l`'s public `Arena` API cannot forge arbitrary `'a` lifetimes or
double-`munmap`, `v4l2::close` cannot close arbitrary fds from safe code, and
dropping streams/devices after unplug or failed dequeue never panics or
double-queues.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fix the unsound public APIs in all three vendored
native crates (`moq-v4l`, `moq-vaapi`, `moq-nvenc`) in `m2` before `0.22.0`
publishes them (rejected: non-breaking internals only in `m1` and deferring
breaking API fixes to `m3`; skipping and tracking upstream only).

1. **Breaking API soundness in `rs/moq-v4l/src/io/mmap/arena.rs:13-17,87-88,95-117`.**
   - `Arena<'a>` exposes `pub bufs: Vec<&'a mut [u8]>` with an unconstrained
     lifetime `'a`, allowing safe code to obtain `&'static mut [u8]` dangling
     pointers after `release()`/`Drop`.
   - `release()` iterates `for buf in &self.bufs` and calls `munmap` without
     clearing `self.bufs`, so a second `release()` or subsequent `Drop`
     double-`munmap`s the same address range.
   - Replace `pub bufs: Vec<&'a mut [u8]>` with private owned RAII mapping
     handles (dropping the phantom `'a` parameter) and borrowed `&self`/`&mut
     self` slice accessors, and `drain(..)` mappings in `release()`.
2. **Non-breaking safety and lifecycle fixes in `moq-v4l`:**
   - Remove `.expect()`/`.unwrap()` panics from `Drop` impls in
     `io/{mmap,userptr}/{arena,stream}.rs` and `device.rs:182` (USB camera
     unplug currently aborts or double-panics).
   - Return `io::Error` instead of panicking on interior NUL in `CString::new`
     at `rs/moq-v4l/src/v4l2/api.rs:65`.
   - Mark `v4l2::close` (`rs/moq-v4l/src/v4l2/api.rs:97`) `unsafe` or
     `pub(crate)` so safe code cannot close arbitrary file descriptors.
   - Retry `EINTR` in `Handle::poll` (`rs/moq-v4l/src/device.rs:156`).
   - Saturate `set_timeout` conversion to `i32` (`rs/moq-v4l/src/io/mmap/stream.rs:139`).
   - Fix `OutputStream::next` (`rs/moq-v4l/src/io/mmap/stream.rs:284-303`) so a
     failed `dequeue` does not leave the buffer double-queued.

Update in-tree callers (`rs/moq-video`) for the new `Arena` API.

Public API: breaking in `moq-v4l` (`Arena` lifetime/field removal,
`v4l2::close` safety/visibility). Wire: none. Add unit tests for `Arena`
release/drop idempotency, `CString` error handling, and `set_timeout`
saturation.
