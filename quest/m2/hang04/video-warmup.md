# [M] Video warmup leaves hang-04, and the players stop reading it

## Goal

moq-hang-04 defines `warmup` for audio renditions only. A video rendition
has no `warmup`, every video group starts with a keyframe again, and no
`version` rule mentions it. No player carries a video warmup path: `@moq/watch`,
`moq_video::decode`, `moq play`, moq-gst, and the bindings read a video
rendition as one whose groups each decode on their own. Audio `warmup`
(Opus pre-roll) and the container consumer's reach that serves it are
unchanged. Video warmup returns in a later catalog version with the
producer that writes it
([the intra-refresh epic](/quest/m4/intra-refresh/README.md)).

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Video `warmup` is dropped from hang-04 and audio `warmup` stays
  (rejected: keep it and make a passing Safari and Firefox run a gate for
  the cut; keep it as is). No producer writes it, two engines have never
  run it, and a 0.22 player that claims catalog version 4 would accept a
  later producer's rendition and mis-decode it instead of refusing it.
- ✅ The same quest deletes the players' video warmup path (rejected: keep
  the path merged with no catalog that can carry the field; close the PR
  that built it).

A correction to the second decision. The question the maintainer answered
said [moq-dev/moq#8857](https://github.com/moq-dev/moq/pull/8857) ("a
warmup rendition plays through one decoder") was open, and the pick read
"delete the path, park #8857". It was not open: it merged 2026-11-15
22:35Z, about ten hours before the question. So there is no PR to park and
no dual decoder left to delete (#8857 removed it). What this quest deletes
is the one-decoder path #8857 landed. The option the maintainer turned
down, "land #8857 anyway, so the one-decoder path exists and is tested
before any producer does", is the state main is in. The pick stands as
recorded, but it was made on a wrong fact, so the PR says this at the top
of its body for the maintainer to confirm or reverse before it merges.

Facts found by the audit:

- No producer sets a video `warmup`: the only writers are the Opus
  importers and encoders. The import that would write it,
  [H.264 import](/quest/m4/intra-refresh/h264-import.md), moved to m4 on
  2026-11-12 for want of a consumer.
- In browsers the path rests on inserting a recovery-point SEI for
  Chromium (`insertRecoveryPoint`, `js/watch/src/video/decoder.ts`).
  Firefox and Safari are unverified, and Chromium refuses a non-IRAP H.265
  frame sent as `key`, so an HEVC refresh rendition fails
  ([second decoder](/quest/m4/intra-refresh/dual-decoder.md)).
- The draft makes video `warmup` above zero one of two triggers for an
  entry `version` of 4 (`VideoConfig`'s `Entry::version`,
  `rs/hang/src/catalog/video/mod.rs`; `videoVersion`,
  `js/hang/src/catalog/root.ts`), and lets a video group start at "a
  picture from which decoding converges within that duration" (the
  Container section's group rules).

For the PR, what goes (find the rest with `grep -rn -i warmup rs js doc`):

- Catalog: `warmup` on `hang::catalog::Video`
  (`rs/hang/src/catalog/video/mod.rs`) and the video schema in
  `js/hang/src/catalog/video.ts`; the warmup trigger in both version
  computations; the MSF mapping for video
  (`rs/moq-mux/src/catalog/msf/consumer.rs`, `js/watch/src/msf.ts`). A
  video `warmup` in a catalog is then an unknown field, ignored like any
  other.
- Browser: the warmup hold in `js/watch/src/video/recovery.ts` (its
  leading-picture refusal stays), `insertRecoveryPoint` and the widened
  subscription in `decoder.ts`, the `config.ts` field,
  `js/watch/bench/warmup.ts` if it measures only video, and the paragraph
  in `doc/lib/js/watch.md`.
- Rust: the warmup in `rs/moq-video/src/decode/consumer.rs`; the video
  buffer's hold in `rs/moq-cli/src/play/{video,buffer,media}.rs`; the video
  `warmup` in moq-ffi (`rs/moq-ffi/src/{video,media}.rs`) and every wrapper,
  and `warmup_us` with `has_warmup` on moq-c's video config
  (`rs/moq-c/src/api.rs`); moq-gst's video branch
  (`rs/moq-gst/src/source/imp.rs`).
- What stays: `moq_mux::container::Playout { delay, warmup }`, the
  consumer's reach in both languages, and every audio use. Check that no
  audio test lost its coverage by leaning on a video case.
- Draft: the `warmup` field section describes audio pre-roll only, and
  loses the sentences about a consumer that "also decodes from an earlier
  group start" and "the earlier decoder's frames", which describe the dual
  decoder #8857 removed. The group rule returns to "a video group MUST
  start with a keyframe". The Version section and the hang-04 changelog
  drop video `warmup`.
- Revert by hand rather than with `git revert`: #8857 also deleted the
  dual decoder (`rs/moq-video/src/decode/select.rs`, 808 lines), which
  must stay deleted.

Tests: a catalog whose video rendition carries `warmup` parses with the
field ignored and no `version`; the players' join and skip tests pass with
the hold gone; the audio warmup tests are untouched.

Public API: breaking. Rust `hang::catalog::Video::warmup`, the video
decoder's warmup options, the moq-ffi and moq-c video `warmup` fields; JS
the video schema's `warmup` and the watch config field. Wire: the hang-04
catalog JSON and group rule, unpublished.

## Related

- [Video warmup returns with its producer](/quest/m4/intra-refresh/catalog-warmup.md) - restores this from history as the next catalog version
