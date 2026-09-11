# [M] moq-audio's Opus encoder follows its bandwidth grant

## Goal

An Opus track published through `moq_audio::encode::Producer` retunes to its
share of the connection's estimate: a grant below the configured bitrate
lowers the encoder at once, room coming back raises it gradually, and a link
too small for the configured audio rate sheds audio bits instead of stalling.
The reservation's ceiling stays the configured bitrate. PCM keeps reserve-only
usage: it claims its fixed rate and never reads the grant.

The rate policy has one home shared by every sender, so audio and video back
off the same way.

## Plan

Today audio reserves but never follows. The capture driver takes the
reservation once the layout reveals the encoded rate
(`rs/moq-audio/src/encode/capture.rs:340`, `:471-474`) and nothing reads its
grant; `Options::bandwidth` documents that
(`rs/moq-audio/src/encode/producer.rs:49-62`). Video already follows through
`rate::Control` (`rs/moq-video/src/encode/producer.rs:467-476`).

- Move `rs/moq-video/src/encode/rate.rs` (`Policy`, `Control`) to
  `moq_mux::rate::Control`. moq-mux is a dependency of both crates
  (`rs/moq-audio/Cargo.toml:82`, `rs/moq-video/Cargo.toml:110`); its `pace.rs`
  is wall-clock delivery of export frames, a different concern. moq-video is
  0.0.23, so deleting `moq_video::encode::rate` ships on main. Update
  moq-video's import (`producer.rs:32`) and the `Options::bandwidth` doc
  (`producer.rs:237`).
- The follow loop lives in `moq_audio::encode::Producer`, not the capture
  driver: `Producer::new` (`producer.rs:255`) already takes `Options` with the
  allocator (`:62`), so it reserves the configured bitrate against
  `self.track().demand()`, holds a `Reservation::consumer()` plus a
  `rate::Control`, and feeds each grant to `Encoder::set_bitrate`
  (`rs/moq-audio/src/encode/encoder.rs:411`). Capture and moq-ffi
  (`rs/moq-ffi/src/audio.rs:254`) build the Producer, so both adapt without
  their own loop; the capture driver's `_reservation` goes away. Public entry
  points for a manual ceiling stay `Producer::set_bitrate` (`producer.rs:295`)
  and `Encoder::set_bitrate`.
- Floor: `set_opus_bitrate` refuses anything outside
  `opus::bitrate_floor(codec_rate, frame_size).max(500)` to
  `300_000 * channels` (`encoder.rs:345-346`,
  `rs/moq-audio/src/opus.rs:137-143`). `Policy::min` defaults to a tenth of
  the ceiling (`rate.rs:55-60`); for Opus it is the codec floor, so a grant
  below it clamps there and never errors. The reservation's ceiling is the
  configured bitrate; only the policy target moves.
- PCM: `pcm::bitrate(sample_rate, channels)` is `pub(crate)`
  (`rs/moq-audio/src/pcm.rs:9`), `Config::bitrate` is refused for it
  (`encoder.rs:269-272`) and so is `set_bitrate` (`encoder.rs:412-414`). A PCM
  Producer reserves its fixed rate and runs no policy. That is the same
  reserve-only usage passthrough imports use, so nothing new is added for it.
- Priority is unchanged: `PRIORITY` puts audio at 80 and video at 60
  (`rs/hang/src/catalog/priority.rs:21-26`), so the allocator fills audio's
  reservation before video sees a bit and audio is squeezed only once the link
  cannot carry audio alone. Worth doing for that tail, not worth blocking on.

Tests: the `Control` unit tests move with the module; an Opus Producer whose
grant drops below its configured bitrate reports the lower `bitrate()` after
one policy step, holds it on a `None` grant, and ramps back when the grant
returns; a grant below the Opus floor clamps at the floor; a PCM Producer
ignores every grant.

This targets 0.0.x crates and needs the allocator on main, so it starts after
the dev merge.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#2848](https://github.com/moq-dev/moq/issues/2848) - close this issue when the quest finishes
