# [XL] Audio follows the clock by its lag alone

## Goal

Both players decide how to play audio from its lag behind the shared clock
and stretch any content, speech, music, or noise, with a continuous time
stretch at a rate proportional to that lag, capped at 10%. Within the
`stretch` threshold audio speeds up or slows down onto the clock; past it, it
snaps; when the audio received ahead of the playhead is about to run out, it
slows down, never trailing past the threshold. In steady state audio rests
within a frame of the clock on every audio-quality row, including `wide` and
`bursty`.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan); the decisions and their prompts
are in the [epic](/quest/m2/audio-playout/README.md).

Why the controller goes: `decide` in `js/watch/src/audio/stretch.ts` and
`rs/moq-audio/src/stretch.rs` is NetEq's `DecisionLogic`. It accelerates only
while the filtered level reaches the target and slows down below three
quarters of it. In NetEq the buffer level is the latency; here the clock is,
and the two agree only when the newest frame arrives on time. Its splices
remove one pitch period per 50 ms cooldown, so the rate follows the talker,
and music or noise that fails admission never moves.

Settled:

- Input: the lag, with the received depth (decoded or still encoded) used
  only to bridge a gap. A target rise or fall and a reference move are lag
  like any other, so the `jumped` accumulation and its one-window rule go,
  and `jumped` leaves `Sink::due` and `bus::Positioned`.
- Rate: proportional to the lag and capped at 10%. The implementer picks the
  gain and reports the time from 200 ms back to within a frame.
- Snap: past the threshold, or for a buffered player past the larger of its
  lookahead and the threshold, skip onto the clock when behind and pad
  silence when ahead. A join snaps. Both rings already do this on NetEq's
  controller (`ring-buffer.ts` `read`, `ring.rs` `skip` and `pad`); keep it.
- Bridge: slow down while the received depth is below a cushion sized from
  the stretch's window and the device read, and the lag is under the
  threshold. The depth, not NetEq's level at arrival, so a batched publisher
  between batches does not trigger it while its next batch is on time.
- Delete NetEq's decision, level filter, cooldown, fast mode, correlation
  admission, pitch-period splices, and their corpus, and the WebRTC license
  files if nothing else ported from WebRTC remains.

Recommendations:

- Chromium's `media/filters/audio_renderer_algorithm.cc` (BSD-3) is a WSOLA
  for arbitrary rates and any content. Implement from a description, or port
  it and carry its license, as the NetEq port did.
- One decision corpus and one stretch corpus both languages consume, as
  today; the spec's "Received-audio time stretch" section is rewritten.
- Fix what the rewrite touches: the worklet drops its clock on every latency
  message (`render-worklet.ts`), so each target change decides unclocked for
  a few quanta; the browser ring starts a frame ahead of the clock (its floor
  is the delay); native's `pad` and `trim` sink options are dead.
- Measure `just test audio-quality` before and after in both players, and
  listen to speech and music at the 10% cap.

Public API: `Sink::due` and `bus::Positioned` lose `jumped`; both are
`main`-only, so nothing published migrates. Wire: none.

## Related

- [WebRTC vendor sync](/quest/m3/vendor-sync/webrtc.md) - tracks the NetEq port this deletes
