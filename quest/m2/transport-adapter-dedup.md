# [S] One copy of the poll transport adapter

## Goal

The poll transport adapter exists once. #4709 copied the same
~310-line adapter into `rs/moq-ffi`, `rs/moq-uring` and `rs/moq-wasm`
(`src/transport/adapter.rs`), and moq-tokio repeats its error and stats
wrappers. The 64 KiB cap on moq-net's default `poll_read_buf` has a unit test.

## Plan

Start from the adapter #4709 landed. The transport-seam quest ruled out a new shared adapter crate.
Revisit that here: share one copy from an existing crate (for example moq-net behind a
feature, or one backend re-exporting it to the others), or record why the
copies stay. Add the `poll_read_buf` cap test either way.

Decided 2026-10-08: backend-specific `moq-quic` methods (`poll_acked`,
`set_deadline`, `set_limits`) stay out of the shared generic adapter; moq-tokio
and moq-uring keep their own adapter code for those. The shared copy covers
only what every backend implements the same way, so a QUIC-only seam never
forces a default onto the browser, wasm, or FFI backends.

Public API: possibly a feature-gated module. Wire: none.

## Related

- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - a backend-specific method the moq-tokio and moq-uring adapters keep
- [Per-stream deadlines](/quest/m2/quic-deadline.md) - adds `set_deadline`, also kept out of the shared adapter
- [QUIC caps](/quest/m2/rate-quic.md) - extends `set_limits` with the bitrate caps
- [Peer limits](/quest/m1/quic/peer-limits.md) - adds the `set_limits` seam
