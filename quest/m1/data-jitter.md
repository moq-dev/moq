# [S] moq-mux: detect jitter on JSON and binary tracks

## Goal

A JSON or binary track whose payloads carry a source time (a sensor sample, a
UDP datagram's arrival) advertises a detected `jitter`: how late the publisher
hands payloads to the transport relative to that time, the same measurement
[jitter clock](/quest/m1/jitter-flush-clock.md) defines for encoders. Tracks
written without a source time advertise none rather than a meaningless zero.

## Plan

- The `moq-binary` and `moq-json` producers stamp each frame with
  `Timestamp::now()` at write, so flush lateness is always zero today. Let a
  payload carry its source timestamp, written as the frame timestamp, without a
  `_with_x` twin of `update`/`append` (for example, accept a type that converts
  from a bare payload).
- Feed the catalog's flush clock from the `moq-mux` data producers when a
  source time is present, and publish the result through the embedded config's
  `Estimate`, as [data sections](/quest/m1/data-sections.md) does for bitrate.
- Mirror the source timestamp in `js/binary` and `js/json` if their producers
  have a caller; otherwise note the gap in the PR.

Public API: additive on `moq-binary`, `moq-json`, and `moq-mux`. Wire: none
beyond the field [data sections](/quest/m1/data-sections.md) adds.

## Required

- [Jitter clock](/quest/m1/jitter-flush-clock.md) - defines the flush-lateness measurement
- [Data sections](/quest/m1/data-sections.md) - adds the `jitter` field and the producers this feeds
