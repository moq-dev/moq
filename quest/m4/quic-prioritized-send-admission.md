# [M] Prioritized send-buffer admission and priority-ordered unblock in moq-quic

## Goal

When `send_window` or peer `max_data` is saturated by low-priority streams (such as a video track), higher-priority streams (such as audio or control) are still admitted without waiting for low-priority data to be ACKed, and freeing send credit wakes blocked writers in priority/group order instead of waking every stream in `connection_blocked` at once.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ include prioritized send-buffer admission among the planned net-new features (rejected: leaving it out of this batch). Joins `m4` as an additive real-time scheduling improvement after the 0.22 cut.

Problem found in the audit (`rs/moq-quic/src/connection/streams/state.rs:78, 1177-1192`, `send.rs:475-499`):

- `Schedule` (two-level priority + DRR send groups) only schedules bytes already admitted to `SendBuffer`. Once `buffered_data >= send_window` (default 10 MB) or `data_sent >= max_data`, admission ignores priority completely, so a single low-priority video stream can fill the entire window and head-of-line block newly opened high-priority audio/control streams.
- `StreamsState::unblock()` pops and wakes *every* stream in `connection_blocked` (`IndexSet`) in LIFO order whenever `write_limit() > 0`, causing a lock thundering herd on saturated relays and letting whichever low-priority task wakes first grab the freed budget.

Guidance:

- Reserve headroom (or enforce per-priority / per-group admission fairness) against `send_window` and `max_data` so higher-priority streams can always write even when lower-priority streams have saturated the buffer.
- Order `connection_blocked` by `(group_priority, stream_priority)` and wake only enough top-priority writers to consume the newly available `write_limit()` instead of waking the entire set.
- Add a multi-stream saturation test (unlike `saturated.rs`, which sets `send_window = u64::MAX` and `max_data = VarInt::MAX`) proving high-priority audio/control writes make immediate progress under a saturated video stream.

Public API: none expected. Wire: none.
