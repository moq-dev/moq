# [S] js/net's reset Announce Stream test closes the session under load

## Goal

`js/net/src/lite/connection.test.ts`'s "a reset Announce Stream closes
moq-lite-07 with PROTOCOL_VIOLATION" passes every loaded `just check`.

## Plan

Found 2026-11-17 by `just check` on
[moq-dev/moq#8996](https://github.com/moq-dev/moq/pull/8996), whose change
touches no JS. Under the loaded check it hit bun's 5000 ms test timeout: the
session never closed. Run alone it passed 15 times in a row.

A timeout, not a wrong code, means the reset went unnoticed. Trace how the
connection reads the Announce Stream when the mock transport delivers the
reset before or alongside the ANNOUNCE_REQUEST it follows: if a path drops the
reset (a read that resolves with the message and never polls again, or a reset
raced against the stream's open), that is a bug in `js/net`, fixed there with
a regression test. Only if the trace shows the close is merely slow is the fix
in the test, by the epic's rules.

Public API: none. Wire: none.
