# [S] A finished PROBE stops reporting its estimate

## Goal

When a PROBE stream ends, because the peer finished or reset it, the Rust
receive estimate reads as unknown instead of reporting the last value for the
rest of the session.

Today `RecvBandwidth` in `rs/moq-net/src/lite/subscriber.rs` gives up after
the stream ends without clearing the estimate, so every `recv_bandwidth()`
consumer keeps the last rate. JS `runProbe` already clears its estimate on
exit.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
review of [#5777](https://github.com/moq-dev/moq/pull/5777).

#5777 has since merged: `RecvBandwidth::poll` now clears the estimate when
the last consumer leaves, but still returns without clearing when the stream
ends. Clear on the stream's end the same way, including moq-tokio's
forwarder across reconnects.

Test: the peer finishes its PROBE stream after one report, and a held
consumer then reads no estimate; it fails without the fix. Wire: none.
