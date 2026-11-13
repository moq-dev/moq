# [S] Interop's finish readers keep their session across a transport fallback

## Goal

`just test interop --all --finish` passes the `finish rust -> *` cells on a
loaded host, and their quarantine in `test/interop/interop.sh`'s
`run_finishes` is removed.

## Plan

Found 2026-11-12 under the flaky-test rule, while iterating
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457), on a host at a
load average near 24. Every `finish rust -> *` cell failed together, and
`just test interop --all --finish` passed all of them on rerun. The Rust
publisher never printed `finish acknowledged` because three readers did not
end clean, so the harness never sent the ack:

- The gst reader connected over the WebSocket fallback, since QUIC was not
  reachable in time, then logged `preferred transport reachable; leaving the
  fallback` and moved to WebTransport mid-test.
- The cpp reader also used the fallback and ended with `health EOF or
  session continuity`.
- The c reader reconnected (`connects=2`) and ended with `finish did not
  drain naturally (frames=0 expected=10)`.

Suspected: a client that leaves the WebSocket fallback for QUIC opens a new
session, which the finish case reads as lost continuity. Decide whether the
move should keep the reader's subscriptions whole (a behavior fix in
`moq_tokio::Connection`) or whether the finish case must pin its transport,
reproduce under load with `INTEROP_QUARANTINED=1` and the logs kept, and fix
the cause, never with a longer limit. Other publishers' finish cells share
the readers, so check whether they fail the same way.

Public API: none expected. Wire: none.
