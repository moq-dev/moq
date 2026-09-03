# [M] moq-audio: nothing in the decode or playback path models a media gap

## Goal

Implement and verify the behavior tracked in [#2981](https://github.com/moq-dev/moq/issues/2981)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

A media gap (a discontinuity in frame timestamps) is treated as contiguous audio everywhere below the catalog. Two reviewers hit different faces of this on [#2968](https://github.com/moq-dev/moq/pull/2968), so filing the shared cause rather than either symptom.

Gaps are routine, not exotic: `latency_max` skipping a stalled group is the common source and predates any of this. `moq play` dropping an undecodable packet (added in #2968) is a second source, and an ingest that resyncs is a third (compare #2798 for the TS side, and #2786 for the video analogue).

#### Where it shows up

**1. `resample::Resampler` concatenates across the gap.** `process` buffers whatever doesn't fill a chunk and prepends it to the next call's input, with no notion of whether that input continues it. Post-gap audio is spliced directly onto pre-gap audio, so the output is shorter than the media timeline it claims, and `decode::Consumer`'s `rewind` (which walks the emitted timestamp back over the buffered frames) is computing from a contiguity that no longer holds.

**2. `moq play` writes to an untimestamped sink.** `play_audio` hands PCM to `playback::Sink`, which just appends. A gap therefore shortens the audio rather than leaving a hole in it. The presentation clock re-anchors from absolute media timestamps on every frame (`media: end - sink.buffered()`), so this self-corrects within one buffer length rather than accumulating, but video runs up to a packet ahead until the hole drains past the speaker.

#### Why it wasn't fixed in #2968

The correct behavior at a discontinuity is to stop carrying samples across it: flush or reset the resampler, and fill the hole with silence so the sink stays aligned with the media timeline. What makes that more than a few lines is deciding *when* a jump counts as a gap, and a too-tight rule is much worse than the bug.

Exact contiguity can't be the test. RTMP carries millisecond timestamps, and an AAC-LC frame at 44.1 kHz is 23.2199… ms, so packets arrive stamped 23, 46, 70, 93 while the true tail advances 23.22, 46.44, 69.66, 92.88. Every packet on the most common ingest path would read as discontinuous, and resetting the resampler per packet destroys exactly the continuity it exists to provide.

So this wants a stated discontinuity policy (tolerance derived from the track timescale and the codec's frame duration, rather than a constant), applied in one place, with tests for both a real gap and the rounding case above.

#### Sketch

- A contiguity test on `decode::Consumer`, comparing the incoming timestamp against the tail it already tracks, with a tolerance that comes from the timescale rather than a magic number.
- On a gap: flush the resampler's pending samples as their own frame (they belong before the hole) or drop them, then reset, and stamp the next output from the new packet rather than by rewinding.
- In `play_audio`: write silence for the missing duration so the sink's contents stay aligned with media time, which also covers `latency_max` skips.
- Tests: packets at frames 0 and 2048 across the gap; and millisecond-stamped 1024-sample AAC packets asserting *no* discontinuity is detected.

Reviewer comments this came from: https://github.com/moq-dev/moq/pull/2968#discussion\_r3826482602 and https://github.com/moq-dev/moq/pull/2968#discussion\_r3826315836

## Closes

- [#2981](https://github.com/moq-dev/moq/issues/2981) - close this issue when the quest finishes
