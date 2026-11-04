# Stream credit by priority

## Goal

When a peer's stream credit runs out, moq gives the streams it can open to
its highest-priority work and makes room for more, instead of refusing or
silently stalling. Every stream a session opens goes through one entry point
that schedules by priority, and a higher-priority group takes over a
lower-priority stream when no credit is left. A JS reader holds hundreds of
live tracks through a browser's ~100-stream WebTransport cap, the most
important ones flowing first.

## Plan

Decided 2026-11-04 (maintainer, planning from Kixel's T3 host, which hit
~100-stream caps: `@moq/qmux`'s default, which `@moq/net` and `@moq/p2p`
now raise to 10,000, and browsers' own,
which JS cannot raise; Chrome stalled at 103, Firefox at 100):

- ✅ Rust and JS, mirrored: the Rust relay is the side that refuses a
  subscription with `NoStreamCredit` (sent as `Internal`, code 0) when its
  publisher runs out of credit. Rejected: JS only.
- ✅ m1, before the cut, because it changes when `NoStreamCredit` is raised
  and what callers see. Rejected: m2.
- ✅ Make room by taking over, else cancelling. Rejected: cancelling only
  (each reset frees credit only after the peer's MAX_STREAMS, a round trip
  later), and ordering waiting opens without making room.
- Detecting the limit needs no library support: an open that is waiting is
  the signal. Browsers keep `createUnidirectionalStream()` pending without
  credit; moq-quic tracks its blocked state (it sends STREAMS_BLOCKED) and
  `@moq/qmux` its `Credit`, and both expose it to the entry point.

This epic owns the end-to-end test: a browser reader and a Rust relay over
WebTransport, 300 live tracks of mixed priority, the highest-priority ones
receiving every group, and nothing hanging.

## Required

- [One priority entry point for streams](/quest/m1/stream-credit/scheduler.md) - every open waits for credit in priority order, and none fails or hangs silently
- [A departed subscription releases its streams](/quest/m1/stream-credit/release-on-leave.md) - on lite-07 a subscription whose reader left frees its upstream streams, so 3 slots serve the next one
- [A higher-priority group makes room](/quest/m1/stream-credit/make-room.md) - it takes over a lower-priority stream, or one is reset, without over-cancelling
