# [S] moq-c handles wrap like PIDs

## Goal

A long-running moq-c host never runs out of handle IDs, and no new handle
ever takes an ID that is still live. Closing a bandwidth consumer documents
that its callback still fires once afterward, like every other cancel path.
The C docs say which thread runs callbacks and that a callback and its
`user_data` must be safe to use from that thread.

## Plan

From the 2026-10-27 audit (FFI-2, FFI-1). `rs/moq-c/src/id.rs` allocates
every handle, including one per delivered frame, from one process-wide
`AtomicU32` and refuses IDs above `i32::MAX`. At 110 frames/s per broadcast,
eight consumed broadcasts (an OBS multiview) exhaust it in about 28 days,
after which every handle-creating call fails. Failed inserts still advance
the counter, so at 2^32 it wraps and `HashMap::insert` silently replaces a
live entry.

Decided 2026-10-27 (maintainer): wrap like PIDs. Keep the monotonic counter,
wrap from `i32::MAX` back to 1, and skip IDs live in the target map. A stale
handle can alias only after about 2^31 intervening allocations, which
preserves the reason
[b0847860bf](https://github.com/moq-dev/moq/commit/b0847860bf) replaced the
reusing slab (parallel tests aliasing each other's freed indices) without the
exhaustion. Rejected: a generation-tagged slab (per-frame churn wraps the
generation within minutes) and removing per-frame handles (hosts may hold a
frame past the callback; breaks the C API). A unit test starts the counter
near `i32::MAX`.

FFI-1: add the "terminal callback fires after close" line to
`moq_bandwidth_consumer_close` in `rs/moq-c/src/bandwidth.rs`, matching its
siblings in `api.rs`.

The 2026-10-28 audit repeated the handle finding (frames, datagrams, and
decoded JSON values each take an ID, so a peer's send rate sets the clock)
and found an unwritten callback contract. `OnStatus` and `OnState`
(`rs/moq-c/src/ffi.rs`) are `unsafe impl Send` over the caller's `user_data`,
and background callbacks run on moq-c's runtime thread, but
`doc/lib/c/index.md` says only "call any function from any thread" and the
SAFETY comments require only that the pointer stay valid. Decided 2026-10-28
(maintainer): document it here rather than in its own quest. Name the
callback thread under Threading in `doc/lib/c/index.md`, add the
thread-safety precondition to the callback typedefs' doc comments (cbindgen
copies them into the header), and state it in the `# Safety` sections of
`OnStatus::new` and `OnState::new` and in each `unsafe impl Send`'s SAFETY
comment.

Public API: doc change only. Wire: none.
