# [L] Native audio playout is a library

## Goal

`moq play` and any other native player place received audio on the shared
clock through one `moq-audio` type, so every rule of the
[audio playout](/quest/m2/audio-playout/README.md) epic reaches each native
player without a copy. `rs/moq-cli/src/play/media.rs` keeps only CLI wiring.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`play_audio` in `rs/moq-cli/src/play/media.rs` (a 2,865-line file with 70
first-parent commits since the last release) carries everything a native
player needs for the epic's rule: anchoring the sink to the shared clock,
resumes across declared ends, declared silence and comfort noise, hole fills,
sink resets, and the 50 ms floor. Its loop keeps nine loose booleans
(`playing`, `ended`, `deferred`, `sequential`, `silent`, `declare`,
`utterance`, `trimming`, `fresh`) instead of one enum, and it matches on
`moq_audio::Error::Decode`, which AGENTS.md forbids. moq-dev/studio's
`moq-scene` keeps a second copy that has already drifted: a 2 s ceiling where
`moq play` uses `moq_mux::container::DELAY_MAX` (10 s), trimming instead of
the lag rule, and no declared-silence or stretch handling.

- Extract the placement into `moq-audio` behind its `playback` feature,
  writing to a `playback::Sink`, with a `bus::Positioned` output for a mixer.
- Model its lifecycle as one enum, and have the decoder report an
  undecodable packet as a `Read` variant instead of an error the caller
  classifies.
- Move `media.rs`'s audio tests with the code; then bump studio so
  `moq-scene` adopts the type and deletes its copy.

It waits so the epic's remaining children land first and this
breaks nothing in the cut.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (b) m3, after the audio playout epic merges (rejected: (a) an m2 child; (c) leave placement in the binary).

Public API: `moq-audio` gains the placement type. Wire: none.

