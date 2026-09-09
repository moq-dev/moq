# [M] Empty frames close the previous frame's duration

## Goal

In the legacy (hang) container every video group ends with an empty frame
stamped at the exclusive end of its last frame, so a group's last frame has a
duration without peeking at the next group, and the LOC consumers are ready
to skip the same marker before LOC producers start writing it. Audio writes no
empty frames: its durations are codec-defined, and the end-of-track marker and
its terminal-packet rule go. CMAF is untouched, since it carries sample
durations. An empty frame is never submitted to a decoder and never means the
track ended.

## Plan

Today only `rs/moq-audio`'s `publish_terminal`
(`rs/moq-audio/src/encode/producer.rs`) writes an empty frame, as the first
frame of a final group followed by the encoder's flush packets, and the
draft's legacy section (`drafts/draft-lcurley-moq-hang.md:514-518`) defines it
as the end marker: consumers discard decoded samples at or after it (the
decode consumer's terminal phase in `rs/moq-audio/src/decode/consumer.rs`,
`js/watch/src/audio/terminal.ts`). `rs/moq-mux/src/container/consumer.rs`
latches `end` (`:186`) sticky from any empty payload and
`js/hang/src/container/consumer.ts` returns it positionally. The last frame of
every group is otherwise timed by a guess:
`rs/moq-mux/src/container/fmp4/fragmenter.rs` times a group's trailing frame
by the catalog cadence because a group boundary is never a duration,
`rs/moq-hls`'s rendition export accumulates whole segments to see each frame's
successor, and the Window-backed timeline from #3240
(`rs/moq-mux/src/timeline.rs`, `end(pts)` at `:1123`) gets a bound only when
a caller passes `cut(Some(end))` (`rs/moq-mux/src/container/producer.rs:216`),
so the archive index records a guessed final duration.

- Draft: the legacy and LOC sections say an empty frame is the exclusive end
  of the frame before it, a video group ends with one, audio has none, and a
  consumer skips it. Remove the end-marker and terminal-packet paragraph.
- Producers: the `rs/moq-mux` container `Producer` writes the marker at `cut`
  and `finish` for video tracks, at the caller's bound or the last timestamp
  plus its estimated duration; the js/hang container producer
  (`js/hang/src/container/legacy.ts:58`) does the same; `publish_terminal`
  stops writing it and its flush packets become ordinary frames, so a few ms
  of encoder padding play at the end unless the consumer trims by the codec
  delay.
- Consumers: the mux consumer records a per-group boundary that feeds
  `max_end`, the last frame's `duration`, and the timeline recorder, instead
  of a sticky `end`; the audio terminal phase goes; js/hang and js/watch
  follow. The fragmenter and the HLS export use the boundary for the trailing
  sample.
- Compatibility: released legacy video consumers already skip empty frames,
  since only the audio decoder reads `end()`, so a legacy publisher is safe
  against an old player, and a new consumer still skips an audio marker from
  an old publisher. Released LOC consumers (`rs/moq-mux/src/container/loc`,
  `js/loc`) submit an empty payload to the decoder, so this quest lands the
  LOC consumer-side skip only; LOC producers start writing the marker in
  [LOC duration marker](/quest/m2/loc-duration-marker.md) once skipping
  consumers have shipped. This lands on main. It ranks in m1 because
  [gap discontinuity](/quest/m1/gap-discontinuity.md) requires it, and the
  dev merge requires that.
- Tests: a group's last frame carries the marker's duration through fmp4
  export and HLS; an audio track end has no marker and plays out; an old-style
  audio marker is skipped; CMAF is unchanged.

## Related

- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - the reset signal, which this marker is not
- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - the forward-only rule the boundary sits under
- [LOC duration marker](/quest/m2/loc-duration-marker.md) - the LOC producer half, gated on a release
