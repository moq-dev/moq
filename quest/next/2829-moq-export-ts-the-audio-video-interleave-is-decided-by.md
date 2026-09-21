# [M] moq export ts: the interleave is a function of the media, not of arrival

## Goal

Two `moq export ts` processes subscribed to the same broadcast emit the same
frames in the same order whenever the broadcast's tracks advance together,
so a redundant pair (SMPTE ST 2022-7) and a regression checksum see one
rendering. Today `pick_next_track` takes the smallest timestamp among the
tracks that *currently hold* a frame, so when audio for `t` has not arrived
but video for `t+1` has, video goes first, and which frame leads is a
property of when bytes reached that process. Measured in #2829: 4.4 % of
slots differ between two legs started at the same instant, on multi-track
content, after #2825 fixed the table cadence.

## Plan

Make the pick a media-time watermark: the earliest pending frame is emitted
only once every unfinished track has shown a frame at or past its timestamp,
so the candidate set is the whole set rather than the arrived part of it.
The bound for a stalled or idle track is `export --max-age` (500 ms by
default, `doc/bin/cli.md`), which already means how long this consumer waits
for a track before moving on; no new flag. A track quiet longer than that is
emitted around, which is the one case two legs may still diverge, and it is
the operator's existing knob.

- `rs/moq-mux/src/container/ts/export.rs`: `fill` already pulls one frame
  into every idle track; `pick_next_track` gains the watermark, reading each
  track's last-seen timestamp (`last_dts` or the pending frame) and the
  `finished` flag. The wait is wall-clock from the moment the leading frame
  became pending, using the existing `max_age`, and `poll_next` registers
  the waiter for it rather than spinning.
- Keep `(timestamp, pid, name)` as the total order once the set is complete.
- A discontinuity on one track still drains the in-hand tail under the old
  generation first (the existing `emit(None)` arm) before the watermark
  applies to the new one.
- `doc/bin/cli.md`: the `export --max-age` line says it also bounds how long
  the muxer holds a leading track for a lagging one.

Tests, in `export_test.rs`: audio for `t` arriving after video for `t+1`
still emits audio first; a track that never delivers past `max-age` is
emitted around and the order resumes once it catches up; two exporters fed
the same frames in different arrival orders produce byte-identical output on
the multi-track fixture; `max-age` zero keeps today's arrival order.

Continuity counters are out of scope: a late-joining exporter cannot know
the packet count of every earlier group, so per-process counters stay
(#2779, closed as won't-fix).

## Closes

- [#2829](https://github.com/moq-dev/moq/issues/2829) - close this issue when the quest finishes
