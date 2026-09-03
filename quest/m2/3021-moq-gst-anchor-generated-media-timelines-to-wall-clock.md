# [M] moq-gst: anchor generated media timelines to wall clock

## Goal

Implement and verify the behavior tracked in [#3021](https://github.com/moq-dev/moq/issues/3021)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

`moqsink` already publishes a Hang companion timeline for each media rendition. The timeline maps media group sequence numbers to presentation timestamps, but its catalog section does not include the optional `wall` anchor.

Without `wall`, consumers can seek by presentation time and locate the live edge, but they cannot translate those timestamps into the publisher's wall-clock domain.

#### Protocol semantics

This issue concerns the Hang companion timeline, not the transport-level TIMESTAMP property.

The MoQ Object Timestamp extension defines:

- A track-level `TIMESCALE`, fixed for the lifetime of the media track.
- An object-level `TIMESTAMP`, containing an absolute presentation timestamp in that timescale.
- Relative timestamp comparisons for relays. It does not define a UTC mapping.

The Hang timeline has its own catalog representation:

- `timescale` is the number of timeline units per second and defaults to 1000.
- Each record's `pts` is the first frame's presentation timestamp, converted into the timeline timescale and rounded down.
- `wall` is the wall-clock time of presentation timestamp zero, expressed in timeline units since the MoQ epoch at `2020-01-01T00:00:00Z`.
- A consumer derives a group's wall-clock presentation time as `wall + pts`.

The media-track and companion-timeline timescales are related by conversion, but they are not the same field and need not use the same units.

#### Current behavior

`moq-mux` already provides the required timeline machinery:

- Generated `<track>.timeline.z` tracks.
- Group-to-PTS records.
- `timeline::Producer::set_wall`, which accepts a media timestamp and its corresponding `SystemTime`.
- Catalog support for the resulting `wall` value.

`moqsink` maps GStreamer PTS through the TIME segment before publishing, but it never supplies a corresponding wall-clock observation to the timeline producer. As a result, the advertised timeline has no `wall` value.

#### Proposed behavior

Establish a wall-clock anchor after receiving the first suitable timestamped media buffer:

1. Use the same mapped media timestamp that is passed to the media importer.
2. Prefer `GstReferenceTimestampMeta` when its reference clock represents an accepted absolute wall-clock domain.
3. Otherwise estimate the corresponding wall time from the local system clock, pipeline clock, base time, running time, and the buffer's TIME-segment mapping.
4. Pass the media timestamp and corresponding `SystemTime` to the existing timeline producer.
5. Republish the rendition's catalog entry using the updated timeline section so consumers receive `wall`.

A reference timestamp from an arbitrary or unidentified clock domain must not be treated as UTC.

The design must also define behavior across timestamp discontinuities. A single `wall` value describes one linear mapping for the entire advertised timeline, so replacing it must not make previously published records resolve against a different epoch.

#### Acceptance criteria

- Existing group-to-PTS timeline records remain unchanged.
- The catalog timeline section includes `wall` after a valid anchor is established.
- For any timeline record, `(wall + pts) / timescale` resolves to the expected duration since the MoQ epoch.
- `GstReferenceTimestampMeta` is preferred only for recognized absolute reference clocks.
- A deterministic local-clock fallback works when no suitable reference metadata is available.
- The implementation preserves the distinction between the media-track timescale and the companion-timeline timescale.
- Timeline integers remain within the draft's `2^53 - 1` limit.
- No transport-level `TIMESCALE` or `TIMESTAMP` semantics are changed.
- Tests cover the reference-timestamp path, the local-clock fallback, catalog publication, and discontinuity behavior.

When the reference timestamp represents capture time, receivers can estimate capture-to-arrival latency. Otherwise, the result is presentation-to-arrival latency.

#### Out of scope

- Changes to transport-level timestamp properties.
- Opaque application tracks.
- Shared timelines or aligned rendition groups.
- Clock synchronization protocols or continuous drift correction.
- Changes to the timeline wire format.

(Co-written by GPT-5 Codex)

## Closes

- [#3021](https://github.com/moq-dev/moq/issues/3021) - close this issue when the quest finishes
