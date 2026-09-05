# [S] moq-net: SUBSCRIBE_ERROR carries a registered error code

## Goal

An IETF subscribe the publisher cannot serve is rejected with the error code
the negotiated draft registers for that reason, never a literal `404`, and
`Error::NotFound`'s wire value agrees with it. The moq-interop-runner's
`subscribe-error` and `subscribe-before-announce` cases pass without the
peer's compatibility branch, which is being removed upstream.

## Plan

Both not-found paths in `rs/moq-net/src/ietf/publisher.rs` call
`reject_subscribe(.., 404, ..)`, and `reject_subscribe` takes a bare `u64`
because SUBSCRIBE_ERROR (`0x05`) has no named code type. `TrackStatusCode`
in `ietf/track.rs` is the wrong registry (TRACK_STATUS). Separately
`Error::NotFound` maps to `13` in `rs/moq-net/src/error.rs`, so one relay
signals not-found as `404` on one path and `13` on the other.

Add a named SUBSCRIBE_ERROR code type mapped to the registered values, and
make it version-aware in the style of the existing `Encode<Version>` /
`Decode<Version>` impls, since the registry moved between draft-14 and
draft-19. Use it at both call sites and route `Error::NotFound` through the
same mapping so the two paths agree. This is the SUBSCRIBE_ERROR half of the
per-protocol code mapping that
[#3001](/quest/m1/3001-ietf-stream-resets-send-moq-lite-error-codes-so-routine.md)
asks for on stream resets; build one table both draw from rather than two.

Tests: the rejection code per negotiated version for a missing broadcast and
a missing track, and the interop-runner cases against a strict peer.

## Closes

- [#3359](https://github.com/moq-dev/moq/issues/3359) - close this issue when the quest finishes

## Related

- [#3001](/quest/m1/3001-ietf-stream-resets-send-moq-lite-error-codes-so-routine.md) - the stream-reset half of the same per-protocol code mapping
