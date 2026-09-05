# [M] play: a real playout clock, with a --delay offset

## Goal

`moq play` presents against a clock it controls. `--delay <duration>` (default
`100ms`) is how far playback trails the live edge, and a frame that arrives
earlier re-anchors playback forward instead of leaving it wherever the first
frame landed.

Today there is no offset anywhere in the path, and two clocks disagree about
what time it is. With audio, `rs/moq-cli/src/play.rs` re-derives `Clock { media:
end - sink.buffered(), wall: now }` on every audio frame, so the offset is
whatever the speaker happens to hold, bounded by a hardcoded `AUDIO_BUFFER_MAX`
of one second. Video-only, `video_clock` is anchored once on the first frame and
never re-anchored (its sole write is gated on `is_none()`), so a first frame that
arrives late appears to leave playback behind live for the rest of the session.
`js/watch/src/sync.ts` handles exactly this by lowering its reference whenever a
frame arrives earlier. Reproduce that case before treating it as a defect.

Not in scope: the `"auto"` (RTT-derived) and `"instant"` modes of `moq-watch`'s
`Delay`, and any FFI surface. `libmoq` and `moq-ffi` hand decoded frames to a
caller callback and have no playout stage, so there is nothing there to delay
yet.

## Plan

Branch from main: nothing here breaks a published API.

### One primitive, not two

`moq_mux::pace::Pacer` is already most of `Sync`. Its `anchor: Option<(Instant,
u128)>` is `Sync`'s `reference`, it re-anchors forward only, and `with_lead` is
nearly `moq-watch`'s `buffer`. The only missing piece is a delay offset, so
extend the primitive rather than adding a parallel one beside it.

It keeps the name `Pacer`. `moq_mux::Clock` is taken by the capture-side shared
epoch (`rs/moq-mux/src/clock.rs`, used across `moq-audio`'s capture path and
`moq-cli publish`), which is a different concept, and the rename's only
justification was the clarity that name would have bought. Staying put also keeps
this additive, so `rs/moq-srt/src/server.rs` and `rs/moq-cli/src/subscribe.rs`
need no change at all.

Three things the offset has to get right, none of which fall out of adding it to
`send_at`:

- **The lead comparison must exclude the delay.** `pace` re-anchors when
  `send_at` leads `now` by more than `lead`, and `lead` defaults to zero, so a
  delay folded into `send_at` makes the very first delayed frame overshoot and
  re-anchor, discarding the offset. `hurry` has the same problem: it returns
  `now`, undelayed, so every later re-anchor drops the delay again. Compare and
  re-anchor on the undelayed instant and apply the delay last.
- **A moved anchor has to wake pending waits.** With audio and video sharing one
  `Pacer`, a task can already be parked waiting on a frame when an earlier frame
  on the other stream moves the anchor. `Sync.#setReference` resolves `#update`
  to wake every parked wait so it recomputes; `Pacer` has no notification at all.

  This is a re-anchor notification, not a timed wait: `kio` has no time module
  (`rs/CLAUDE.md`), so a `poll_*` that had to fire at a deadline would park
  forever with nothing to arm it, and taking a `moq_net::Timers` the way
  `origin::Driver::run` does would drag a runtime into a type that has never
  needed one. Keep `pace` returning the instant and let the caller sleep, which
  is the contract the export path already relies on, and add only the
  `poll_*`/`kio::Waiter` pair that reports the anchor moving.
- **Audio must not take the delay twice.** If audio waits for the delayed
  instant and then hands samples to a sink that also holds a delay's worth, it
  trails video by roughly another delay. Either feed the ring ahead of the
  deadline and let it only update the shared anchor, or subtract the sink depth
  from the audio write deadline.

### play.rs

- Delete the local `Clock` struct and `video_clock`, and route both audio and
  video through the one `Pacer`. The reference anchor becomes the single
  authority; the speaker-derived clock goes away.
- Honoring a delay in audio means configuring the sink, not the throttle.
  `AUDIO_BUFFER_MAX` in `play.rs` only gates writes against `sink.buffered()`;
  the ring itself is sized by the private `LATENCY` (50ms) and `CAPACITY` (3s)
  constants in `rs/moq-audio/src/playback/sink.rs`, so one of them has to become
  configurable. Keep a floor the way `ringSamples` does in
  `js/watch/src/audio/latency.ts`: a zero-depth ring can never be read from.
- `--delay <duration>` replaces `--max-age` on `play`, with no alias. The two are
  one number here, since nothing older than `delay` is worth presenting, so
  `delay` is what goes on the wire as the subscription's max age. `--max-age`
  stays unchanged on `import`, the stdout containers, and `rtmp export`.
- Reconcile `doc/bin/cli.md`, which documents `play --max-age` and carries an
  example invocation using it, plus any other `moq play` example in the tree.

### Open questions

Left open deliberately: they are A/V policy that wants the code in front of it,
and the answer changes what the tests should assert.

- **Which stream owns the anchor when both exist.** Video taking a tune-in burst
  can `hurry` the shared anchor past what the speaker is still draining, and
  because the anchor only moves forward, later audio cannot pull it back. Making
  the speaker the sole re-anchor source while audio exists is one answer;
  discarding audio to the same edge is another.
- **What the speaker contributes.** `last_timestamp - buffered()` is the sample
  sounding now, so feeding it in as the anchor while every result is shifted by
  `delay` would schedule video a full delay behind it. If the speaker position
  stays in the calculation it has to enter at the live edge, not the playing
  edge.
- **Whether a per-write bound is needed.** `play_audio` splits PCM into chunks of
  up to a second and checks `sink.buffered()` only between whole writes, so a
  first chunk can overshoot a 100ms delay outright. Sizing each part to the
  remaining headroom is the obvious fix; whether the sink should grow an
  operation for it is not.

### Verification

- Unit tests on `Pacer` with `tokio::time::pause()`: a late first frame followed
  by an earlier one re-anchors forward; a frame that merely arrives late does
  not; the delay survives a re-anchor through both `pace` and `hurry`; a wait
  parked on one stream shortens when another stream moves the anchor; and
  `delay = 0` paces exactly as today, which is what keeps the export path
  unchanged.
- Manually: `moq play` against the demo relay, with audio and video-only,
  checking that a late start catches up to live rather than staying behind it,
  and that audio and video stay aligned at a non-zero delay.

## Required

- [#2981](/quest/m0/2981-moq-audio-nothing-in-the-decode-or-playback-path-models-a.md) - settles `play_audio`'s gap and silence handling first, so the clock rework is not rebased onto it

## Related

- [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - the same anchoring model in js/watch, gaining an absolute wall-clock target
