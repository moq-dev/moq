# [S] largest is a getter

## Goal

Reading a track producer's largest position is a plain read in Rust and JS.
No caller polls a stream with a no-op waiter to fake a getter, and a second
JS caller can never hang because another consumed a shared cursor's update.

## Plan

Decided 2026-10-22 (API audit). Rust `track::Producer::largest(&mut self)`
(`rs/moq-net/src/model/track.rs`) is a stream on the writer; five production
callers use `poll_largest(&Waiter::noop())` as a getter (moq-mux
`catalog/producer.rs`, `ts/programs.rs`, moq-video `encode/producer.rs`). JS
`Track.Producer.largest()` (`js/net/src/track.ts`) keeps the cursor on the
shared handle; reproduced: one caller consumes an update and an independent
second caller waits forever.

- Rust: `largest(&self) -> Option<Position>`, built on the private
  `latest_sequence`. A caller that needs to wait uses the consumer side.
- JS: the same getter; if a waiting form has a consumer, it returns a fresh
  cursor per call instead of sharing one.

Public API: breaking in moq-net and `@moq/net`. Wire: none.
