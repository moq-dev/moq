# [M] Surface fatal media ingestion failures and release the decoder

## Goal

Every fatal audio/video setup, read, or decode failure reaches watch's existing
error output and tears down its decoder/subscription. Cancellation and source
changes retain their existing semantics.

## Plan

P2. Swallowed ingestion errors also exist on release. Main's new fatal error
latch in [moq-dev/moq#5227](https://github.com/moq-dev/moq/pull/5227) covers
WebCodecs callbacks but leaves these read paths uncovered.

At main 6369eca, nested `Effect.spawn` tasks in
`js/watch/src/video/decoder.ts:598,688` and `audio/decoder.ts:452,552` reject
into the generic logging catch in `js/signals/src/index.ts:788`. An outer
CMAF setup catch cannot catch a later nested task rejection. A malformed
container or synchronous configure/decode error can stop ingestion while
leaving the player apparently live and its public `out.error` unset.

A Bun reproduction used real Broadcast/Track/Container consumers, published
an empty legacy video frame missing its timestamp varint, mocked only
WebCodecs, and drained microtasks. Result: one read, one logged error,
`out.error === undefined`, and zero decoder closes. Audio has the same
source structure but was not independently runtime-reproduced.

Route fatal failures through the existing latch and scoped teardown in both
media types and all supported container paths, including Legacy/LOC/CMAF.
Test malformed reads and synchronous decoder errors as well as callback
errors, preserving cancellation and source-replacement tests. No new API or
wire change is expected: make the existing output truthful. Current consumers
are watch players and applications observing decoder errors. Consider a
release-compatible fix after main for the inherited stall behavior.
