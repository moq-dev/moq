# [XS] @moq/net's lite subscribe tests wait for the whole request

## Goal

The three `lite %s: a subscribe goes out with its track` cases (lite-05,
06, and 07) in `js/net/src/lite/subscriber.test.ts` pass when every budget
poll is forced to yield. They decode the SUBSCRIBE only after the request
has been fully written.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7663](https://github.com/moq-dev/moq/pull/7663)): milestone m0,
a child of the test-flakes epic. #7663 fixes the announce harness only and
leaves these tests alone.

- The test (`subscriber.test.ts:1289`) waits on `(await
  opened(1)).writing`, which resolves on the first written chunk. It then
  decodes `streams[1].written`. The `fakeSession` fixture (`:1015`) copies
  the bytes after one host turn, so under forced yields the SUBSCRIBE can
  still be partial.
- Wait on a signal that the request is complete, such as a full-message
  promise from the fixture, then decode. Don't wait on the subscriber reading
  its response, as #7663 does for the announce harness: on lite-05 and later
  the subscriber reads it only after TRACK_INFO, which the test sends after
  decoding SUBSCRIBE.
- Verify under forced yields (#7663 shows how to force them) and at normal
  speed. Check `fakeSession`'s other users for the same partial-write
  wait.

Public API: none. Wire: none.
