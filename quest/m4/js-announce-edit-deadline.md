# [S] js/net closes a session whose Announce Stream stalls

## Goal

A lite-07 `js/net` session whose peer stops reading its Announce Stream closes
after 10 seconds without write progress while messages wait, as the Rust session
does and the draft requires, and a test on mocked time shows it.

## Plan

From [the route stream](https://github.com/moq-dev/moq/pull/8835). The draft
closes a session whose Announce Stream made no progress for 10 seconds with
messages left to send. Rust arms `Stall` for `route::path::HOLD` only while a
flush is pending (`rs/moq-net/src/lite/subscriber/route_stream.rs`), closing with
`announce stream stalled`; `js/net/src/lite/announce_stream.ts` waits forever.

- Mirror the Rust write-stall deadline and its close code in `js/net`.
- An edit the peer reads but never answers with ANNOUNCE_OK has no deadline in
  Rust, `js/net`, or the draft. Recommendation: leave it so. Withholding the
  answer stalls only that session's discovery, which the peer can already do by
  sending nothing, and a timer per edit buys no behavior a well-behaved peer
  needs.
- Decide with the same change whether the Announce Stream opens eagerly, as
  Rust does, or with its first message, as `js/net` does today, and make the
  two match if a reason to differ is not found.

Public API: none. Wire: none.
