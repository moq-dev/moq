# [XS] moq-quic's long header type is one variant

## Goal

`LongType`, left with only `Handshake` after
[moq-dev/moq#7492](https://github.com/moq-dev/moq/pull/7492) moved 0-RTT out,
collapses into a `Handshake` header variant, and `LongHeaderType::Standard`
goes with it.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). A mechanical change in `rs/moq-quic/src/packet.rs`. Keep the packet fuzz
target and its regression seeds passing.
On main, `LongType` still has `ZeroRtt` until #7492 lands.

## Required

- [moq-quic deletes its remaining 0-RTT code](/quest/m1/quic-zero-rtt-leftovers.md) - moves 0-RTT out of `LongType`, in [moq-dev/moq#7492](https://github.com/moq-dev/moq/pull/7492)
