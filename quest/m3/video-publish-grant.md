# [S] A published capture exposes its bandwidth grant

## Goal

A caller that publishes a capture through moq-video can read the bitrate the
bandwidth allocator grants that track, not only the encoder's target, so a UI
shows each track's share of the uplink.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

moq-net already exposes the grant (`bandwidth::Reservation::rate`, also in
moq-ffi), but `encode::publish_capture` (`rs/moq-video/src/encode/producer.rs`)
holds the reservation privately in its capture loop and returns nothing. The
only figure a caller sees is the target reported through the catalog stats.

Consumer: the studio's encode-control rungs and Health panel
([moq-dev/studio#25](https://github.com/moq-dev/studio/pull/25)). Recommendation: return a handle from the publish
call that carries the grant, rather than a callback.
