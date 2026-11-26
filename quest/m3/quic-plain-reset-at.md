# [S] moq-quic resets plainly toward a peer without RESET_STREAM_AT

## Goal

`moq_quic` `send::Stream::reset_at` toward a peer that did not negotiate RESET_STREAM_AT sends RESET_STREAM at once, instead of holding it until the prefix is acknowledged, and `qmux` does the same toward a peer without the parameter. A stream reset that way frees its send state and credit immediately.

## Plan

Picked 2026-11-26 (maintainer, on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)).

The hold exists so a reset still delivers its prefix, but a plain RESET_STREAM lets the peer discard data it has not read, even acknowledged data, so the hold guarantees nothing. Since [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626), `moq-net` holds a GROUP header only when `poll::Session::reliable_reset` is true and names any other reset group in `SUBSCRIBE_DROP`, so nothing relies on the hold.

Change `reset_to` in `rs/moq-quic/src/connection/streams/send.rs` (and the matching QMux path), update the `reset_at` docs there and on `moq_net::transport::poll::SendStream::reset_at`, and replace the tests that assert the hold (`reset_at_retains_prefix_until_acked` and its kin in `streams/state.rs`) with ones asserting the immediate reset. Measure the send state a reset stream holds before and after.

## Required

- [Replace lite-07 SUBSCRIBE_SKIP and stall timers with Previous Delta](/quest/m2/lite07-subscribe/previous-delta.md) - stops `moq-net` relying on the hold
