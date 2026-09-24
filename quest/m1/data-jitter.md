# [S] moq-mux: detect delay and jitter on JSON and binary tracks

## Goal

A JSON or binary track whose payloads carry a capture time on the publisher's
own clock (a UDP datagram's arrival, a sensor read) advertises a detected
`delay` and `jitter`: how late the publisher hands payloads to the transport
relative to that time, the same measurement
[jitter clock](/quest/m1/jitter-flush-clock.md) defines for encoders. A
telemetry source slower than the video it accompanies shows up as `delay`.
Tracks written without a capture time advertise neither rather than a
meaningless zero.

## Plan

- The `moq-binary` and `moq-json` producers stamp each frame with
  `Timestamp::now()` at write, so flush lateness is always zero today. Let a
  payload carry its capture timestamp, written as the frame timestamp, without a
  `_with_x` twin of `update`/`append` (for example, accept a type that converts
  from a bare payload).
- The capture time must come from the publisher's clock, the one
  `Timestamp::now()` reads. A device-native clock (a flight controller's boot
  time) is an unrelated epoch that would report nonsense and, through the
  broadcast-wide baseline, distort every other track; it stays in the payload.
  Reject a timestamp ahead of `now` rather than clamp it.
- Add optional `delay` to `JsonConfig` and `BinaryConfig` in `rs/hang`,
  `js/hang`, and the draft, beside the `jitter`
  [data sections](/quest/m1/data-sections.md) adds.
- Feed the catalog's flush clock from the `moq-mux` data producers when a
  capture time is present, and publish the result through the embedded config's
  `Estimate`, as [data sections](/quest/m1/data-sections.md) does for bitrate.
- Mirror the capture timestamp in the published `js/binary` and `js/json`
  producers, so browser publishers can produce the same timed tracks.

Public API: additive on `hang`, `moq-binary`, `moq-json`, `moq-mux`,
`@moq/hang`, `@moq/binary`, and `@moq/json`. Wire: one optional field on data
entries.

## Required

- [Jitter clock](/quest/m1/jitter-flush-clock.md) - defines the flush-lateness measurement and `delay`
- [Data sections](/quest/m1/data-sections.md) - adds the `jitter` field and the producers this feeds
