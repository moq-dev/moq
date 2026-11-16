# [S] An unwatched video rendition rests

## Goal

A `moq_video::encode::Producer` rendition with no `lower` stops spending on
frames nobody watches: while its track has no subscriber it does not encode at
its full ceiling, and the next viewer still joins on a keyframe without a burst
of frames encoded at a rate the grant would not have funded.

## Plan

Accepted 2026-11-16 (maintainer, on
[moq-dev/moq#8964](https://github.com/moq-dev/moq/pull/8964#issuecomment-6931517209)):
#8964 lifted the capture driver's rule into `encode::Producer`, so moq-ffi's
`MoqVideoProducer`, moq-c's `moq_encode_video*` and moq-boy now also encode an
unwatched rendition at its ceiling (`Producer::rest` in
`rs/moq-video/src/encode/producer.rs`), where the bindings used to follow the
grant regardless. Its groups wait in the track cache, so the cost is encoder
work and a join that starts on a group encoded at the ceiling. m3, as a fix to
behaviour #8964 shipped.

Recommended: while unwatched, close the encoder like a disabled rung but keep
the rendition advertised enabled; the first write after a subscriber arrives
reopens it, which opens on a keyframe on every backend, at the rate the grant
then funds. A simulcast rung keeps following the grant as it does now, since it
needs the grant's slice to resume. `publish_capture` keeps reporting its state.

Tests: an unwatched producer encodes nothing and holds no encoder; a viewer
arriving gets a group starting on a keyframe at the granted rate; a viewer
leaving and returning does the same.

## Waiting

- Maintainer: how an unwatched rendition rests. Options: (1) recommended, close
  the encoder until a subscriber arrives, which saves the most and costs the
  first viewer one encoder open; (2) keep encoding at the policy floor (a tenth
  of the ceiling), which keeps a group cached for an instant join but still
  encodes every frame; (3) keep encoding at the last granted rate, which is
  what the bindings did before #8964.
