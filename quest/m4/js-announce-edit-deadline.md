# [S] js/net closes a session whose peer never answers an Announce Stream edit

## Goal

A lite-07 `js/net` session whose peer leaves an ANNOUNCE_REQUEST edit
unanswered closes, as the Rust session does, and a test on mocked time shows
it.

## Plan

From [the route stream](https://github.com/moq-dev/moq/pull/8835). An edit is
stop-and-wait, so a peer that withholds ANNOUNCE_OK holds every later edit of
the session's prefixes. Rust arms a deadline of `route::path::HOLD` while its
stream has unanswered or unwritten bytes (`rs/moq-net/src/lite/subscriber/route_stream.rs`);
`js/net/src/lite/routed.ts` waits forever.

- Mirror the Rust deadline and its close code in `js/net`.
- Decide with the same change whether the Announce Stream opens eagerly, as
  Rust does, or with its first message, as `js/net` does today, and make the
  two match if a reason to differ is not found.

Public API: none. Wire: none.
