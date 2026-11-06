# [M] Media crate tests run on a mocked clock

## Goal

The media and bridge crates' tests that wait on real time move to a paused
or simulated clock, or an awaited event. See the
[epic](/quest/m1/mock-clock-tests/README.md) for the rules.

## Plan

As of 2026-11-06:

- moq-audio, 9: `tests/integration/playback.rs` 5, `src/aec.rs` 2, and
  `src/playback/driver.rs` 2.
- moq-video, 8: `src/decode/consumer.rs` 3, `src/encode/producer.rs` 3,
  `src/capture/wgc.rs`, and `src/frame/cuda_test.rs`.
- moq-hls, 8: `src/export/mod.rs` 7 and `src/import.rs`.
- moq-mux, 5: `src/rate.rs`.
- moq-rtc 3 (`src/lib.rs`, `src/server/whep.rs`), moq-rtsp 3
  (`src/client.rs`), moq-srt 1 (`src/server.rs`), and moq-transcode 1
  (`src/lib.rs`).

Tests that drive real devices or encoders keep real time where the device
needs it, and say so.

Public API: none. Wire: none.
