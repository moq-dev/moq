# [XS] A request stream that opens after its deadline is aborted once and never sends

## Goal

A `@moq/net` IETF request whose stream finishes opening only after its
deadline fired is aborted exactly once, through its `RequestStream`, and
never writes its request. A test proves it.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7355](https://github.com/moq-dev/moq/pull/7355).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- [moq-dev/moq#7355](https://github.com/moq-dev/moq/pull/7355) (merged) routes a deadline failure through the owning `RequestStream`
  (`js/net/src/ietf/request_stream.ts`, `js/net/src/util/deadline.ts`), so a
  timed-out request is ended once. Its online review suggested one more test,
  left as this follow-up: resolve the opening stream only after the deadline
  expires, then assert the owner aborts it exactly once and the request bytes
  are never written.
- Add it beside `request_stream.test.ts`'s "a request's deadline ends it
  through the request" case. Fix the code if it fails.
- The deleted quest of the same name (one 10 s deadline from create to
  answer) planned this reset; this quest only proves it through the new
  owner.

Public API: none. Wire: none.
