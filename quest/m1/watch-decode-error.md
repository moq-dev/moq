# [S] A WebCodecs error stops playback loudly

## Goal

When a video or audio decoder in `@moq/watch` errors, the track's subscription
ends and the element reports an error state, instead of video silently
stopping or audio decoding nothing while its subscription stays open.

## Plan

Today the video decoder's `error` callback (`js/watch/src/video/decoder.ts`,
around line 404) closes the track effect with a `// TODO bubble up error`, and
`#runActive` keeps the dead track. Audio logs the error, then its decode loop
`break`s while the subscription stays open.

Decided (2026-10-07): surface the error and stop, rather than resubscribe. A
deterministic decode error would just loop under resubscription, and the repo
rule is to fail loudly. Both decoders end the subscription and set one visible
error signal that the element exposes. Reuse an existing error or status
signal on the element if there is one; otherwise propose the name in the PR.
Update `doc/` wherever the element's states are documented.

Public API: likely one new or widened error/status signal on `@moq/watch`.
Wire: none.

## Related

- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - removes one trigger of this error, not the handling
