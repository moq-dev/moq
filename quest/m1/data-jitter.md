# [S] moq-mux: detect delay and jitter on JSON and binary tracks

## Goal

A JSON or binary track whose payloads carry a capture time on the publisher's
own clock (a UDP datagram's arrival, a sensor read) advertises a detected
`delay` and `jitter`: how late the publisher hands payloads to the transport
relative to that time, the same measurement `moq_mux::catalog::Estimator`
makes for encoders. A
telemetry source slower than the video it accompanies shows up as `delay`.
Tracks written without a capture time advertise neither rather than a
meaningless zero.

## Plan

- The `moq-binary` and `moq-json` producers stamp each frame with
  `Timestamp::now()` at write, so flush lateness is always zero today. Let a
  payload carry its capture timestamp, written as the frame timestamp, without a
  `_with_x` twin of `update`/`append` (for example, accept a type that converts
  from a bare payload).
- The capture time must be on the broadcast's `moq_mux::Clock`, the timeline
  media PTS use. `Timestamp::now()` has its own jittered per-process epoch, so
  a data frame stamped with it cannot be compared with media; map the capture
  instant through the broadcast clock, in a type that cannot be mistaken for
  an unmapped `Timestamp`. A device-native clock (a flight controller's boot
  time) is an unrelated epoch that would report nonsense and, through the
  broadcast-wide baseline, distort every other track; it stays in the payload.
  Reject a timestamp ahead of the clock's `now` rather than clamp it.
- Add optional `delay` to `JsonConfig` and `BinaryConfig` in `rs/hang`,
  `js/hang`, and the draft, beside `jitter`.
- Feed the catalog's flush clock from the `moq-mux` data producers when a
  capture time is present and a frame was actually emitted. A `moq-json`
  snapshot `update` with an unchanged value succeeds without writing one, so
  the lower producer reports whether it emitted, and a repeated value must not
  move the baseline (cover it in tests). Publish the result through the embedded config's
  `Estimate`, as the data producers already do for bitrate.
- Have the lower producers also report each emitted frame's encoded size, and
  measure bitrate from that instead of the pre-compression payload or
  serialized value: today an unchanged snapshot `update` still counts, and
  DEFLATE can slightly expand an incompressible payload.
- Mirror the capture timestamp in the published `js/binary` and `js/json`
  producers, so browser publishers can produce the same timed tracks.

Public API: additive on `hang`, `moq-binary`, `moq-json`, `moq-mux`,
`@moq/hang`, `@moq/binary`, and `@moq/json`. Wire: one optional field on data
entries.

