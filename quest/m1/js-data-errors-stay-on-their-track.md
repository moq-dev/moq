# [S] A JS hop's data decode error never closes another session

## Goal

In `@moq/net`, an error from decoding a Group or Fetch Stream aborts only that
group on each copy of its track, never the track, its broadcast, its sibling
groups, or a session. Today `Reader` marks every
error a decoder throws as malformed (`stream.ts`, `decodeError`), and the
mark travels with the error object: a node that republishes a track aborts
its copy with that object, and its publisher's `runSubscribe` then closes the
downstream session with "invalid control encoding" (`lite/error.ts`
`closeViolation`). One bad group from one publisher drops every broadcast a
JS relay or forwarding client serves on that session.

## Plan

Found by [moq-dev/moq#9266](https://github.com/moq-dev/moq/pull/9266): a
lite-05/06 frame whose timestamp passes 2^53 milliseconds, such as a
scale-less track's key as Rust and now JS stamp it, fails `toNumber` in
`lite/group.ts` `readFrames`, and the `group-id.test.ts` relay chain then saw
the viewer's session close with code 3.

Recommended: keep the malformed mark on control-stream decodes only, or
strip it where a data reader turns the error into a group abort
(`lite/subscriber.ts` `runGroup`, the fetch reader), so `closeViolation`
sees a plain error downstream. Check the Rust relay for the same leak, and
cover it with a JS relay-chain test that feeds one malformed group and keeps
a second broadcast on the downstream session flowing.
