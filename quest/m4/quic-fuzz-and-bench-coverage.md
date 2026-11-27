# [M] Close moq-quic fuzzing, real-crypto benchmark, and sim coverage gaps

## Goal

Every untrusted parser and modern `moq-quic` state-machine feature has fuzz coverage (including QMux, `ACK_RECEIVE_TIMESTAMPS`, `Endpoint::handle`/`Token::decode`, and the full `Connection` API surface), and `rs/moq-quic/benches/` includes real `rind` crypto benchmarks alongside `Plaintext` plus wire/QMux/Endpoint DoS benches.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ plan all verified audit bugs, latency, CPU, and coverage work (rejected: bugs and real-time latency only; drop). Joins `m4` as test, fuzzing, and benchmark coverage after the 0.22 cut.

Gaps identified in the audit:

- **QMux fuzz target (`rs/moq-quic/fuzz/`)**: Add a libFuzzer target covering `mux::wire::next_frame`, `mux::params::Params::decode`, and `mux::Connection` across `QMux00`, `QMux01`, and `QMux02` (framing, closes, resets, datagrams, records).
- **`frame` and `params` fuzz seeds (`rs/moq-quic/src/fuzz.rs:248-254, 323-358`)**: Add `ACK_RECEIVE_TIMESTAMPS` (`0x03178307` / `0x03178308`) to frame seeds and iterate `ack.receive_timestamps()` in `fuzz::frame`; add `max_receive_timestamps_per_ack` and `receive_timestamps_exponent` to `params` seeds.
- **Widen `connection` fuzzer (`rs/moq-quic/src/fuzz/connection.rs`)**: Add operations for `send::Group` (open/close/reprioritize/join/grouped datagrams), `Lease` (`poll_lease`/`restore`/`release`/`set_window`), `send_rate`/`receive_rate`, `set_probe_target`, `quiesce`/`abandon_quiesce`, `ack_frequency`, `receive_timestamps` (non-`None` `arrival`), ECN codepoints, path migration/`local_address_changed`, and handshake packet loss/reordering.
- **`Endpoint` & token fuzz target**: Add a fuzz target driving `Endpoint::handle` (random short/Initial/Retry packets, `Token::decode`, `CidQueue`/`CidState`, `start_accept`/`finish_accept`).
- **Real-crypto & missing micro-benchmarks (`rs/moq-quic/benches/`)**: Add real `rind` AEAD + header-protection benchmarks (not just `fuzz::Plaintext`), plus benches for `ACK_RECEIVE_TIMESTAMPS` encode/decode, `STREAM` frame codec, QMux record/split/assembler, `Endpoint::handle` miss/Initial DoS path, and `BloomTokenLog` lookup/conversion.
- **Sim workload gap**: Add a 1-RTT key-update-under-loss/reordering workload to `rs/moq-quic/src/tests/sim/workloads.rs`.

Public API: none. Wire: none.

## Related

- [Benches move from Criterion to rust2-bench-harness](/quest/m5/bench-harness.md) - match whichever bench harness is active when this runs
