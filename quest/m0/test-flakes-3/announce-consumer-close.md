# [S] Closing a JS announce consumer ends the peer's stream under load

## Goal

`moq-lite-07-wip: closing an announce consumer ends the peer's announce stream`
in `js/net/src/integration.test.ts` passes under a loaded `just check`.

## Plan

Seen once on 2026-11-19 while validating
[moq-dev/moq#9139](https://github.com/moq-dev/moq/pull/9139), which touches no
`js/net` code: `until(() => ended === i)` hit its 1 s wall-clock deadline
during the full `test js` run, and the test passed alone right after.

Find whether the peer's announce stream really ends late under load (the
close reaching the server only on a later turn) or whether the 1 s `until`
deadline is what fails. Then assert on the stream's end as an event instead of
polling against wall time.

Public API: none. Wire: none.
