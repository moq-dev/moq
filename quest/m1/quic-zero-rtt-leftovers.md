# [XS] moq-quic deletes its remaining 0-RTT code

## Goal

moq-quic has no 0-RTT code left beyond what it needs to recognize and drop a 0-RTT packet: no Data-space mapping for 0-RTT, no early-data guards or tests, and a test proving a peer's 0-RTT packets are dropped unprocessed.

## Plan

The maintainer deleted 0-RTT entirely (see [quic-api](/quest/m1/quic-api/README.md)): MoQ needs the finished handshake for CONNECT and authorization. [moq-dev/moq#6837](https://github.com/moq-dev/moq/pull/6837) already drops 0-RTT before decryption (`rs/moq-quic/src/endpoint.rs:215-227`, `rs/moq-quic/src/connection/packet_crypto.rs:16-19`), so the "ACK in 0-RTT" report on [moq-dev/moq#7334](https://github.com/moq-dev/moq/pull/7334) isn't reachable on main.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run): a standalone m1 cleanup. Remove what's left:

- `PartialDecode::space()` and `Header::space()` mapping 0-RTT to the Data space, and `is_0rtt()` (`rs/moq-quic/src/packet.rs:82-95`, `:391-393`), keeping only the `LongType::ZeroRtt` decode needed to skip the packet.
- The no-op `LongType::ZeroRtt` arm in `rs/moq-quic/src/connection/mod.rs:2812-2814`.
- The rustls early-data guards and their test (`rs/moq-quic/src/crypto/rustls.rs:295-296, 355-356, 420-421, 458-459, 605-681`) if rustls's defaults already leave early data off; otherwise keep the guard and its test.
- The characterize test `early_packets_leave_handshake_and_application_data_usable` (`rs/moq-quic/src/tests/characterize/frames.rs:38-110`) becomes a test that a 0-RTT packet, ACK included, is dropped; fix the comment at `rs/moq-uring/tests/integration/endpoint.rs:290`.

`process_early_payload` handles Initial and Handshake packets, not 0-RTT; leave it.

Public API: none. Wire: none.
