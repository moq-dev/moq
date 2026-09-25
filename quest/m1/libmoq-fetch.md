# [S] libmoq: fetch one cached group

## Goal

A C embedder can fetch one cached group by sequence through the existing frame,
handle, and terminal-status conventions. The new entry point is additive.

## Plan

Mirror `MoqTrackConsumer::fetch_group` from `rs/moq-ffi/src/consumer.rs` in
`rs/libmoq`, supporting raw and container-decoded delivery as the FFI does.
Specify ownership, cancellation, cache misses, and terminal delivery using the
existing consume contracts. Test a hit, miss, and cancelled fetch from a C caller.

Regenerate `moq.h` and update `doc/lib/c/index.md`, whose capability list already
claims group fetch. Decoder output configuration landed separately on the dev line.
The `moq_group_request_*` tests in `rs/libmoq/src/test.rs` fetch from Rust for
lack of this entry point; switch them to it.
