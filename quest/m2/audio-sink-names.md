# [XS] The audio sink's silence markers say what they declare

## Goal

`moq_audio::playback::Sink` declares a publisher's announced silence and the
start of the next utterance under names that say so (recommended:
`silence()` and `resume()`), not `end()` and `next()`. `end()` no longer sits
beside `finish()` on a type it does not end.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Rename the two methods before the cut (rejected: keep; hide the sink's
  placement methods until the playout library lands). A name is free to
  change only until moq-audio publishes with it.

Facts found by the audit:

- `Sink::end()` (`rs/moq-audio/src/playback/sink.rs:228`) declares silence
  the publisher announced; it does not end the sink, which `finish(self)`
  does (`:150`). `Sink::next()` (`:238`) starts the next utterance.
- The publisher calls the same marker `discontinuity`
  (`rs/moq-audio/src/encode/producer.rs:668`), and the decoder reports it as
  `Read::End`.
- One caller: `moq play`, four call sites in `rs/moq-cli/src/play/media.rs`
  (`:889`, `:900`, `:912`, `:1029`) through the output trait at
  `rs/moq-cli/src/play/output.rs:59`, `:62`. No binding, GStreamer element,
  or OBS source calls them.

For the PR: settle the names against the publisher's `discontinuity` and the
decoder's `Read::End`, so the three read as one idea, and rename the trait
methods in `rs/moq-cli/src/play/output.rs` to match. The sink's placement
methods (`output_latency`, `set_latency`, `due`) stay as they are:
[the playout library](/quest/m3/audio-playout-library.md) decides their
future after the cut (2026-11-14).

Recommended in the same PR, not decided by the maintainer:
`encode::Producer::reanchor()` (`rs/moq-audio/src/encode/producer.rs:366`)
and `discontinuity()` (`:668`) are a lazy and an immediate marker for one
idea, and moq-ffi exposes only `reanchor`. Keep one public method, or say in
the docs why a caller needs both.

Public API: moq-audio breaks (two method names). Wire: none.

## Related

- [Native audio playout is a library](/quest/m3/audio-playout-library.md) - builds on the sink after the cut
