# [M] Fix moq-quic stream, Assembler, QMux, BloomTokenLog, CidState, rate-cap, and 32-bit wire bugs

## Goal

All remaining verified correctness and panic bugs from the `moq-quic` audit are fixed with targeted regression tests: `Assembler` discards already-read duplicates on compaction and clamps `end` on `truncate`, stale `pending.reset_stream` never trips `debug_assert!`, empty writes emit no 0-byte `STREAM` frames, `Send::acked()` is monotonic across `reset_acked`, QMux close encoding never underflow-panics, 32-bit wire decoders reject oversized VarInts instead of truncating, `BloomTokenLog` uses the right `k_num` and period anchor, `CidState::on_cid_timeout` keeps unretired expiration entries, `send_rate`/`receive_rate` always refill before charging/granting, and `validate_receive_id` checks `sent_max_remote`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m1` as additive correctness fixes.

Caused by: [moq-dev/moq#7494](https://github.com/moq-dev/moq/pull/7494) (`mux/mod.rs:594`), [moq-dev/moq#7094](https://github.com/moq-dev/moq/pull/7094) (`send_rate`/`receive_rate` stale `Bucket` refill in `packet_builder.rs:201-203` and `streams/state.rs:1364`), [moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950) (`streams/send.rs:287-297, 475-509, 662`); the rest are inherited from `quinn-proto`.

Fixes to land with unit tests:

- **`Assembler::insert` & `truncate` (`assembler.rs:171, 207-222`)**: Call `self.defragment(self.bytes_read)` instead of `self.defragment(0)`, and clamp `self.end = self.end.min(end)` in `truncate()`.
- **Stale `pending.reset_stream` (`spaces.rs:334`, `streams/state.rs:741-755`)**: Filter `reset_stream` in `Retransmits::is_empty()` to streams still present in `streams.send` (matching `max_stream_data` and `stream_data_blocked`) so a lost `reset_at` requeued after a `STOP_SENDING`-shrunk reset was ACKed never triggers an empty ack-eliciting packet or `debug_assert!` panic.
- **Empty `poll_write` & `Send::acked()` (`streams/send.rs:287-297, 475-509, 662`)**: Return `Ok(Some(0))` without pushing into `Schedule` or waking transmit when `data.remaining() == 0` (and without blocking/queuing `DATA_BLOCKED`); in `Send::acked()`, check `offset <= self.acked_len()` (when `!self.peer_stopped()`) before `offset > self.pending.offset()`.
- **QMux `fill_into` close underflow (`mux/mod.rs:584-596`, `frame.rs:294-305, 339`)**: Use `saturating_sub` and exact varint sizes in `ConnectionClose::encode` / `ApplicationClose::encode`, and check `budget.saturating_sub(buf.len())` in `mux::Connection::fill_into` (spilling the close frame to a second record if `QX_TRANSPORT_PARAMETERS` left less than `Close::SIZE_BOUND`).
- **32-bit wire truncation (`frame.rs:779`, `packet.rs:56, 608`)**: Replace `get_var()? as usize` with `usize::try_from(...)` so `1 << 32` fails decode cleanly on 32-bit ARMv7 targets instead of truncating to `0`.
- **`BloomTokenLog` (`bloom_token_log.rs:32-48, 70-96`)**: Pass `max_bytes / 2` to `optimal_k_num` (matching `filter_max_bytes`, halving wasted hashes from 58 to 29 and fixing the doc comment), and anchor `period_1_start` cleanly (e.g. `expires_at - lifetime` or epoch-aligned period) so valid tokens issued slightly before the first redeemed token are not rejected.
- **`CidState::on_cid_timeout` (`cid_state.rs:89-121`)**: Only pop `retire_timestamp` when `!unretired_ids_found` so pending expirations aren't dropped permanently.
- **`send_rate` / `receive_rate` stale refill (`packet_builder.rs:201-203`, `streams/state.rs:1364`)**: Ensure `Bucket::refill(now)` runs before `charge()` on all packet paths (including coalesced/close/probe) and pass `now` (or refill) on `receive_rate` grants.
- **`validate_receive_id` (`streams/state.rs:1229`)**: Check remote stream IDs against `sent_max_remote` rather than unannounced `max_remote` (consistent with `stream_limit_checked_against_announced_value`).

Public API: none. Wire: none.
