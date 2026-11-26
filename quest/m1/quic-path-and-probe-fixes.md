# [M] Fix dual-stack NAT rebinding, prev_path rollback, MTUD gating, bandwidth-probe panics, and ACK-only spurious loss

## Goal

IPv4-mapped IPv6 and IPv6 NAT port rebindings preserve path RTT, congestion state, and MTU; `prev_path` `PATH_RESPONSE` probes work and release old path state once validated; MTUD probes respect cwnd, pacing, `send_rate`, and full-size path validation; bandwidth probes never panic on ACK-filled packets or falsely report `cwnd_limited`; and lost ACK-only packets never trigger `congestion.on_spurious_loss()`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m1` as additive correctness and real-time latency fixes.

Caused by: [moq-dev/moq#7546](https://github.com/moq-dev/moq/pull/7546) / [moq-dev/moq#8957](https://github.com/moq-dev/moq/pull/8957) (bandwidth probe interactions in `connection/mod.rs:793-897, 4032-4041`); dual-stack `remote.is_ipv4()`, `prev_path` handling, MTUD gating, and ACK-only spurious loss are inherited from `quinn-proto`.

Fixes to land with regression tests:

- **Dual-stack NAT rebinding (`connection/mod.rs:3706-3708`)**: Replace `remote.is_ipv4() && remote.ip() == self.path.remote.ip()` with `remote.ip().to_canonical() == self.path.remote.ip().to_canonical()` so IPv4 NAT rebinds on dual-stack IPv6 sockets (`::ffff:a.b.c.d`, default in `moq-tokio`) and IPv6 port rebinds preserve RTT/cwnd/MTUD instead of resetting and stalling >=1 RTT. Add a dual-stack test in `tests/characterize/paths.rs`.
- **`prev_path` validation & cleanup (`connection/mod.rs:3452-3474`)**: Handle `Frame::PathResponse` matching `self.prev_path` (RFC 9000 §9.3.3 spoofed-migration rollback), and clear `self.prev_path = None` once `self.path` validates at full size so the old `PathData` + controller aren't leaked.
- **MTUD probe gating (`connection/mod.rs:1276-1310`)**: Gate MTU probes on `!send_blocked && !rate_blocked && self.path.challenge.is_none()` and check cwnd, pacer, and `send_rate` (including `refill(now)`) before sending, so probes don't inject unpaced traffic into congested bottlenecks or probe above `min_mtu` during a short-challenge re-validation.
- **Bandwidth probe empty-packet panic & attribution (`connection/mod.rs:793-897, 3798, 4032-4041`)**: Prevent `maybe_ack_non_eliciting()` from filling a bandwidth-probe packet before `bandwidth_probe.write()` (or gracefully skip `bandwidth_probe` when no room remains), and do not set `cwnd_blocked` / `pacing_until` when a bandwidth probe (not application data) hits cwnd or pacing.
- **Spurious loss for ACK-only packets (`connection/mod.rs:2083-2103, 2429-2434`)**: Only record packets that reported loss to the congestion controller (`info.congestion.is_some()`, scoped to `path_generation`) in `lost_packets`, so late-arriving ACK-only packets cannot call `on_spurious_loss()` and undo an unrelated real congestion cut.
- **Small timer & stats cleanups**: Order `Timer::Idle` ahead of `Timer::LossDetection` (`timer.rs:26-36`), clear stale `Timer::Pacing` when unblocked, and use `saturating_duration_since` at `connection/mod.rs:2370` and `spaces.rs:674`.

Public API: none. Wire: none.
