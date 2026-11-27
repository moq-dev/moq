# [S] Fix stuck_waiting and Chunks::finalize_inner receive-window eviction deadlocks

## Goal

A connection with a full receive window never deadlocks with all readers parked: `recv::Stream::restore()` always runs stuck-window eviction when the window is full, `Chunks::finalize_inner()` triggers eviction if returned credit is absorbed by `receive_window_shrink_debt`, and `rs/moq-net/verify/credit/ReceiveCredit.tla` models `stuck_waiting`, shrink debt, and mixed plain-read + lease streams so regressions fail model checking.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m1` as an additive correctness fix to `main`'s credit rework before the 0.22 cut.

Caused by: [moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950) (`rs/moq-quic/src/connection/streams/lease.rs:413-419, 690-768`; `rs/moq-quic/src/connection/streams/recv.rs:603-633`).

Failure modes found in the audit:

- `evict_if_stuck()` resets `stuck_waiting = false` and calls `trim_unlooked()`, which iterates `self.unlooked` (`FxHashSet`, arbitrary order), breaks on the first live unlooked stream, and sets `stuck_waiting = recv.lease.out`.
- If `trim_unlooked()` stops on a stream with `ready == true, out == false` (either a single stream that then leases, restores, and parks, or alongside another `out` stream), `restore()` skips `self.unstick()` because `stuck_waiting == false`, deadlocking the full receive window.
- When a plain `read()` stream drains via `Chunks::finalize_inner()` while `receive_window_shrink_debt` absorbs the returned credit (`window_max_data <= data_recvd`), eviction is never re-run.
- Shrinking `receive_window` at runtime (`streams/state.rs:1296-1312`) also skips `max_data_due()`, `evict_if_stuck()`, and re-checking parked `poll_lease` readers whose `want` exceeds the shrunk window.

Guidance:

- Remove the `stuck_waiting` gate in `restore()` (`evict_if_stuck()` already early-returns in O(1) when `window_max_data > data_recvd`) and call `evict_if_stuck()` in `Chunks::finalize_inner()` and `set_receive_window()` when `window_max_data <= data_recvd`.
- Add unit tests covering all three deadlocks and extend `rs/moq-net/verify/credit/ReceiveCredit.tla` (`verify-credit` skill) to model mixed plain-read/lease streams and shrink debt.

Public API: none. Wire: none.

## Related

- [Lease ownership and `Default` removal](/quest/m2/quic-lease-ownership.md) - tags `Lease` with `(StreamId, ConnectionTag)`
