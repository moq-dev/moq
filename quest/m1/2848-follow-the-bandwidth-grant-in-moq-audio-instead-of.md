# [M] moq-audio's Opus encoder follows its bandwidth grant

## Goal

An Opus track published through `moq_audio::encode::Producer` retunes to its
share of the connection's estimate: a grant below the configured bitrate
lowers the encoder at once, room coming back raises it gradually, and a link
too small for the configured audio rate sheds audio bits instead of stalling.
The reservation's ceiling stays the configured bitrate. A grant below the
codec floor disables the rendition (catalog `enabled: false`) until it recovers. PCM keeps reserve-only
usage: it claims its fixed rate and never reads the grant.

The rate policy has one home shared by every sender, so audio and video back
off the same way.

## Plan

Today audio reserves but never follows. The capture driver takes the
reservation once the layout reveals the encoded rate
(`rs/moq-audio/src/encode/capture.rs`) and nothing reads its
grant; `Options::bandwidth` documents that
(`rs/moq-audio/src/encode/producer.rs`). Video already follows through
`moq_mux::rate::Control` (`rs/moq-video/src/encode/producer.rs`).

- Consume `moq_mux::rate`, keeping one shared policy; this quest adds audio
  adaptation without growing a second implementation.
- The follow loop lives in `moq_audio::encode::Producer`, not the capture
  driver: `Producer::new` already takes `Options` with the
  allocator, so it reserves the configured bitrate against
  `self.demand()`, holds a `Reservation::consumer()` plus a
  `rate::Control`, and feeds each grant to `Encoder::set_bitrate`
  (`rs/moq-audio/src/encode/encoder.rs`). Capture and moq-ffi
  (`rs/moq-ffi/src/audio.rs`) build the Producer, so both adapt without
  their own loop; the capture driver's `_reservation` goes away. Public entry
  points for a manual ceiling stay `Producer::set_bitrate`
  and `Encoder::set_bitrate`.
- The Opus encoder sits behind the encode backend seam: the rate setter and
  its floor live in `rs/moq-audio/src/encode/backend/libopus.rs`, and
  `Encoder::set_bitrate` dispatches through the backend.
- Floor: the libopus backend's `set_bitrate` refuses anything outside
  `opus::bitrate_floor(codec_rate, frame_size).max(500)` to
  `300_000 * channels` (`backend/libopus.rs`, `rs/moq-audio/src/opus.rs`). `Policy::min` defaults to a tenth of
  the ceiling (`rs/moq-mux/src/rate.rs`); for Opus it is the codec floor. A
  grant below it disables the rendition until bandwidth recovers (decided
  2026-10-04 with the enabled flag); the encoder never errors. Once viewers
  deselect it, its demand and so its grant go away, so recovery evaluates a
  hypothetical share against the current estimate, as the ladder controller
  does. The reservation's ceiling is
  the configured bitrate; only the policy target moves.
- PCM: `pcm::bitrate(sample_rate, channels)` is `pub(crate)`
  (`rs/moq-audio/src/pcm.rs`), `Settings::bitrate` is refused for it
  and so is `set_bitrate` (`encoder.rs`, `backend/pcm.rs`). A PCM
  Producer reserves its fixed rate and runs no policy. That is the same
  reserve-only usage passthrough imports use, so nothing new is added for it.
- Priority is unchanged: `PRIORITY` puts audio at 80 and video at 60
  (`rs/hang/src/catalog/priority.rs`), so the allocator fills audio's
  reservation before video sees a bit and audio is squeezed only once the link
  cannot carry audio alone. Worth doing for that tail, not worth blocking on.

Tests: retain the shared `Control` tests; an Opus Producer whose
grant drops below its configured bitrate reports the lower `bitrate()` after
one policy step, holds it on a `None` grant, and ramps back when the grant
returns; a grant below the Opus floor disables the rendition, and recovered
bandwidth enables it again even after the last viewer unsubscribed; a PCM Producer
ignores every grant.

## Closes

- [#2848](https://github.com/moq-dev/moq/issues/2848) - close this issue when the quest finishes
