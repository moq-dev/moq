# [L] Bindings finish a broadcast cleanly

## Goal

A binding app whose input ended cleanly can finish its broadcast through
`moq-ffi` and every binding, so subscribers still catching up drain the
finished tracks until their media is stale. An interrupt ends the wait early.
`close` keeps its abrupt meaning.

## Plan

Decided (2026-10-16), mirroring the Rust library:

- `finish()` is a call next to `close()` that waits for the linger, as
  `broadcast::Producer::finish` does in Rust. Cancelling the wait is the
  interrupt and closes at once. Rejected: a linger helper that each app
  holds and sleeps on, because every publisher would repeat the hold, and an
  origin-held linger, because it ties the session to what the origin holds.
- Tracks without a max-age add nothing to the wait. The alternative,
  `cache_duration`, defaults to `MAX` and would linger forever.

Expose it as an async method on `MoqBroadcastProducer` and map cancellation
to each language's idiom (a dropped future, a cancelled task or coroutine).
The C ABI has no futures, so check how `moq-c` already shapes async calls
and follow it. Report the API shape across languages before landing.

Follow the Cross-Package Sync row for `rs/moq-ffi`: hand-written wrappers
in py, swift, kt, go, dart, c and cpp, plus the `doc/lib` pages. Check
whether `cpp/obs` should finish on a clean stop.

Test in `moq-ffi` with a mocked clock: a late subscriber within max-age gets
the media, the broadcast closes once it passes, and cancelling closes early.
Each binding needs at least a smoke test that the call exists and returns.
