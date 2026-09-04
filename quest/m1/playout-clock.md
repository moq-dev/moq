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

Branch from dev: renaming a published `moq-mux` item is a semver break.

### One clock, not two

`moq_mux::pace::Pacer` is already most of `Sync`. Its `anchor: Option<(Instant,
u128)>` is `Sync`'s `reference`, it re-anchors forward only, and `with_lead` is
nearly `moq-watch`'s `buffer`. The only missing piece is a delay offset, so
extend the primitive rather than adding a parallel one beside it.

- Rename `Pacer` to `Clock` (`moq_mux::Clock`), since it now serves playout as
  well as export pacing. Update the three call sites, which all keep `delay = 0`:
  `rs/moq-srt/src/server.rs` (two) and `rs/moq-cli/src/subscribe.rs`.
- Add the offset: `send_at = anchor + (ts - base) + delay`.
- Add the pair the rest of the crate uses: `poll_wait(&kio::Waiter, ts) ->
  Poll<()>` with an `async fn wait(ts)` wrapping it, per the Async / poll
  plumbing section of `rs/CLAUDE.md`.

### play.rs

- Delete the local `Clock` struct and `video_clock`, and route both audio and
  video through the one `moq_mux::Clock`. The reference anchor becomes the single
  authority; the speaker-derived clock goes away.
- Size the audio sink to `delay` rather than the hardcoded `AUDIO_BUFFER_MAX`,
  mirroring `ringSamples(rate, delay)` in `js/watch/src/audio/latency.ts`. Keep a
  floor the way `ringSamples` does: a zero-depth ring can never be read from.
- `--delay <duration>` replaces `--max-age` on `play`, with no alias. The two are
  one number here, since nothing older than `delay` is worth presenting, so
  `delay` is what goes on the wire as the subscription's max age. `--max-age`
  stays unchanged on `import`, the stdout containers, and `rtmp export`.

### Verification

- Unit tests on `Clock` with `tokio::time::pause()`: a late first frame followed
  by an earlier one re-anchors forward; a frame that merely arrives late does
  not; the delay is honored; `delay = 0` paces exactly as `Pacer` did, which is
  what keeps the export path unchanged.
- Manually: `moq play` against the demo relay, with audio and video-only,
  checking that a late start catches up to live rather than staying behind it.

## Required

- [#2981](/quest/m0/2981-moq-audio-nothing-in-the-decode-or-playback-path-models-a.md) - settles `play_audio`'s gap and silence handling first, so the clock rework is not rebased onto it

## Related

- [#2278](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - the same anchoring model in js/watch, gaining an absolute wall-clock target
