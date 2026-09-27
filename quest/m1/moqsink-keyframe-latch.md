# [XS] moqsink waits for a published keyframe after a break

## Goal

A video pad in `moqsink` survives a pause or segment break whose first buffer
carries only codec headers. Today `Media::write` in
`rs/moq-gst/src/sink/pad.rs` clears its `keyframe` latch after any successful
decode. A header-only buffer decodes without emitting a frame, clears the
latch, and the next delta frame hits `MissingKeyframe` outside the guarded
arm, which permanently invalidates the pad
([#4239](https://github.com/moq-dev/moq/pull/4239)).

## Plan

Keep the latch set until the importer actually publishes a keyframe, not
until the first buffer parses. That needs `import::Track::decode` to say
whether it emitted a frame, or a comparable signal; prefer reusing what the
importer already knows over inferring it in the pad.

Regression test beside `video_pause_drops_deltas_until_the_next_keyframe`: a
break followed by a header-only buffer (SPS/PPS alone for H.264), then a
delta, then a keyframe. The delta drops and the keyframe publishes; the pad
stays valid.
