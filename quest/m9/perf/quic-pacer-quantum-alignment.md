# [S] Align moq-congestion pacer bursts with BBR3 send_quantum and check relay MAX_ACK_ONLY and Dedup bounds

## Goal

On 1–50 Mbps real-time video streams, `moq-congestion`'s `Pacer` no longer accumulates 10 ms / 10-packet micro-bursts that BBR3's 1 ms `send_quantum` then splits into 8–9 un-GSO'd single-packet syscalls at `t = 0`; and high-bitrate relay links (>100 Mbps) don't lose RTT samples from `SentPackets::MAX_ACK_ONLY = 256` or drop reordered packets past `Dedup`'s 129-packet window.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m9/perf` as an internal pacing and relay-link tuning quest with no public API or wire change, gated on bufferbloat sim and bench measurements.

Findings from the audit:

- **Pacer vs BBR3 quantum mismatch (`rs/moq-congestion/src/pacer.rs:226-263`, `bbr3/mod.rs:1616-1622`, `rs/moq-quic/src/connection/mod.rs:646-652`)**: At 10 Mbps (1452 B MTU), `Bbr3::burst()` returns `send_quantum = 1452` (clamping `max_datagrams = 1`, zero GSO), while `Pacer::rate_capacity` allows `MIN_BURST_SIZE = 10` / `MAX_BURST_INTERVAL = 10 ms` (~8–9 packets = 12.5 KB). The driver loop then emits 8–9 single-packet syscalls back-to-back at `t = 0` (inflicting 10 ms queuing jitter with zero GSO savings) and sleeps 10 ms. Same formula is duplicated in `bandwidth_probe.rs:367-371`. Also clamp `bytes_to_send` to `self.capacity` in `pacer.rs:108, 136` (matching `delay_at_rate` at line 193).
- **Relay-scale `MAX_ACK_ONLY` and `Dedup` (`sent_packets.rs:15`, `spaces.rs:567`)**: `MAX_ACK_ONLY = 256` covers only ~40 Mbps receive at 100 ms RTT before ACK-only packets are forgotten (costing RTT samples); `Dedup` tracks 129 packets (`u128`), which wide ECMP/GRO reordering on high-BDP relay trunks can overrun. Measure whether scaling these (or sizing `MAX_ACK_ONLY` to BDP/receive rate) improves high-rate relay RTT stability without adding hot-path cost.
- **MTU black hole threshold (`mtud.rs:430, 508`)**: Evaluate lowering `BLACK_HOLE_THRESHOLD` (currently requires 4 PTO bursts) on sim links with sudden MTU drops so live streams don't stall for 4 PTOs.

Measure p99 latency, queuing delay, and syscalls/s on `moq-quic`'s bufferbloat sim (1, 10, 50, 500 Mbps) before and after.

Public API: none. Wire: none.

## Related

- [Realtime congestion control](/quest/m4/quic-realtime.md) - BBR3 delay bound, GCC, SCReAMv2 bake-off
- [Packet, ACK, and syscall count tracking](/quest/m4/quic-packet-count-metrics.md) - monitors syscalls and packet ratios
