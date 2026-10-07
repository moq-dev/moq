# [S] One copy of the poll transport adapter

## Goal

The poll transport adapter exists once. #4709 proposes copying the same
~310-line adapter into `rs/moq-ffi`, `rs/moq-uring` and `rs/moq-wasm`
(`src/transport/adapter.rs`), and moq-tokio repeats its error and stats
wrappers. The 64 KiB cap on moq-net's default `poll_read_buf` has a unit test.

## Plan

Blocked until transport seam (#4709) lands; confirm the landed adapter
shape first. The transport-seam quest ruled out a new shared adapter crate.
Revisit that here: share one copy from an existing crate (for example moq-net behind a
feature, or one backend re-exporting it to the others), or record why the
copies stay. Add the `poll_read_buf` cap test either way.

Public API: possibly a feature-gated module. Wire: none.
