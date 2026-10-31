# [S] An ended lite track with an unsettled tail aborts at session death

## Goal

When a session dies after a lite publisher ended a track but before its tail
settled, Rust and JS both abort the track, and both end it cleanly once the
tail settled. Both agree on what declares the end.

## Plan

Found while landing [moq-dev/moq#6969](https://github.com/moq-dev/moq/pull/6969).
Rust lite marks a track ended on the subscribe stream's FIN, on every
version, and its cleanup then calls `finish()` without checking the tail:
`SubscriptionCleanup::abort` and `close`, and `ServeMode::Tail`'s
`ServeEnd::Finished`, in `rs/moq-net/src/lite/subscriber.rs`. JS declares
the end on SUBSCRIBE_END (and, after #6969, the FIN on lite-01..04), and
`track.Producer#close` downgrades an abort to a clean end only once the
declared end settled. Rust's IETF subscriber already aborts.

Decided 2026-10-28 (maintainer):

- Rust aborts like JS, since `finish()` on an unsettled tail reports
  incomplete content as complete. Its cleanup always calls `abort` and relies
  on `track::Producer::abort`'s existing downgrade when the declared end
  settled. So does `ServeMode::Tail` when the session closes before the tail
  settles: that branch returns `ServeEnd::Finished` today, which
  `TrackServeRun` turns into `serving.finish()`, so it must abort instead.
- Align the end signal in both languages: SUBSCRIBE_END on lite-05+, the
  stream FIN on lite-01..04. On lite-07 a bounded range also FINs, which must
  not end the track.

Cover it with a test in each language for the settled and unsettled cases,
including Rust's `ServeMode::Tail` session close, and an interop case that
kills the session with groups unsettled.

Public API: none. Wire: none; behavior only. A Rust consumer of an
unsettled track sees the session error instead of a clean finish.
