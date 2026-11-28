# [S] A JS IETF publisher never closes its session for an upstream's violation

## Goal

In `@moq/net`, a moq-transport session that serves a track closes only for its
own peer's protocol violations, never for one another session put on the
track. Today `ietf/publisher.ts` rethrows any `ProtocolViolation` serving
raised (the `publishError` rethrow after the serve race, and the outer
`catch` that rethrows violations), so a track a lite or IETF upstream aborted
with a violation, such as a group past its declared end, closes every
downstream IETF session that forwards it.

## Plan

Mirror the lite publisher fix from the PR that finished
`js-data-errors-stay-on-their-track`: only errors from this session's own
decodes (updates on the request stream) reach the connection's violation
path. A served track's error ends only that subscription. Also update
`runDatagrams` (`js/net/src/lite/publisher.ts:2998`) to wrap with
`servedError(error(err))` instead of passing raw `error(err)` to
`stream.abort` (found in the 2026-11-28 audit of
[moq-dev/moq#9829](https://github.com/moq-dev/moq/pull/9829)). Cover it with a
relay-chain test like `js/net/src/data-errors.test.ts`, over an IETF
downstream, where the upstream track is aborted with a `ProtocolViolation`, plus
a lite-07 datagram downstream case verifying `runDatagrams` resets with a
malformed-track error while the downstream session stays open.
