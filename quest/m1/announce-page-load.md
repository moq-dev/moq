# [S] The live marker waits for the first connection on page load

## Goal

An announcement stream opened before the first session connects does not
report `live` until that session's replay lands, or the connection attempt
gives up. Today it has no session to wait on, so it goes `live` at once and
the broadcasts arrive after it, in both `@moq/net` and `rs/moq-net`.

## Plan

- Open question for the maintainer: what counts as giving up (the first failed
  attempt, the backoff ceiling, or never).
- The JS reconnect loop (`js/net/src/connection/reload.ts`) already counts as
  an answerer for requests through `expect()`; it can also take a replay hold
  on the origin until its first session lands or it gives up.
- Rust has no reconnect loop of its own; decide whether an app that opens a
  stream before connecting needs an equivalent hold there.
- Add a caught-up test that opens the stream before the first connection.
