# [S] moq-audio: held resampler frames keep their source timestamp

## Goal

`decode::Consumer` stamps resampled output where its first sample came from,
in every phase, instead of subtracting held-frame counts from the newest
packet's timestamp. A forward gap can no longer place audio up to a packet
away from where it belongs, and `Frame::activity`, keyed by the same stamp,
labels the right span.

## Plan

`starts_at` in `rs/moq-audio/src/decode/consumer.rs` rewinds the current
packet's `decoded_at` by `pending` input frames and `skipped` startup frames;
`Resampler` in `rs/moq-audio/src/resample.rs` exposes only counts
(`pending_frames`, `skipped`), never where the held samples came from. #3386
mitigated the reported case on both branches: `Consumer::read` detects an
undeclared hole before decoding (`discontinuous(next, mux_frame.timestamp)`),
drains the resampler through `gap()`, and stamps the tail from the pre-gap
`tail`. Two holes remain: the gap check is skipped once `self.end` is set (the
terminal phase), and a jump inside `discontinuous()`'s slack still rewinds
across silently.

- The resampler records the source timestamp of the oldest held input frame,
  fed alongside the samples, and `starts_at` reads it; the `skipped` startup
  frames rewind from that.
- Delete the derived subtraction; `pending_frames` stays for the drain
  accounting.
- Regression: a 10 ms packet at 0 leaves 10 ms held, the next packet lands at
  1 s, and the output is stamped from 0 rather than near 990 ms, in the
  terminal phase too, with the activity span labelled from the held packet.
  Cover a jump inside the slack.
- [Duration marker](/quest/m2/duration-marker.md) removes the audio end marker
  and the terminal phase; whichever lands second simplifies the other.

## Closes

- [#3326](https://github.com/moq-dev/moq/issues/3326) - close this issue when the quest finishes

## Related

- [Duration marker](/quest/m2/duration-marker.md) - removes the terminal phase this quest has to cover
