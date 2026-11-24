# [L] Import CEA-608/708 captions as text tracks

## Goal

The H.264 and H.265 importers take the CEA-608/708 captions a source carries
in its video's SEI and publish each caption channel as a catalog text track
with `role: "caption"`, so every player gets captions as an ordinary text
track and none parses SEI. The video track no longer carries them: each SEI
NAL loses only its A/53 caption message and keeps every other one, and a NAL
left with no message is dropped.

## Plan

Decided 2026-11-23 (maintainer, on
[moq-dev/moq#9443](https://github.com/moq-dev/moq/pull/9443#issuecomment-7091545924)):
✅ the importer strips caption SEI and publishes a `role: "caption"` text track
(rejected: players parse SEI on decode, which that PR adds to `@moq/watch`).
Decided 2026-11-23 (maintainer, `/quest-run`): ✅ plan it here (rejected:
leaving it to that PR's author). That PR's `cc_data` parser and tests
(`js/watch/src/video/ccdata.ts`) are a starting point for the decode step.
[moq-dev/moq#9442](https://github.com/moq-dev/moq/issues/9442) asks for the
rejected player-side option, an opt-in `DecoderInput.emitSei`; this quest
answers its need, so it closes the issue (decided 2026-11-23, maintainer,
/quest-import).

Extract in the codec importers (`rs/moq-mux/src/codec/h264/import.rs`,
`rs/moq-mux/src/codec/h265/import.rs`), which see every container's frames in
both framings and already scan SEI for HDR metadata
(`rs/moq-mux/src/codec/color.rs`). Walk each SEI NAL with `sei_messages`
(`rs/moq-mux/src/codec/annexb.rs`) and take the ITU-T T.35 registered
messages (payload type 4) whose A/53 `GA94` user data holds `cc_data`. Frames
arrive in decode order, so order the `cc_data` by each frame's presentation
time before decoding, holding back at most the stream's reorder depth.

Decode 608 into `vtt` cues: pop-on gives a cue per displayed caption, and
roll-up and paint-on give a cue per row as it is displayed, ending when it
scrolls off or is cleared, with the row and column as WebVTT `line` and
`position` settings. Decode 708 services the same way. Publish every 608 channel
and 708 service that carries data: broadcasters usually send the same
captions in both, but a 608 channel may carry a language no 708 service
does, and nothing in the stream proves two match, so dropping either can lose
captions the importer has already stripped from the video. Label each track
with its channel or service (`CC1`, `Service 1`) so a menu tells the copies
apart, and take the language from a caption service descriptor when the
container carries one (MPEG-TS's PMT). Add a track when its channel first carries data, as the
catalog already grows mid-stream.

Rewriting an SEI NAL re-serializes the remaining messages with their sizes
and re-applies emulation prevention, and a length-prefixed frame updates the
NAL's length. Triples with `cc_valid` 0 are padding, and a 608 byte with bad
parity decodes as the standard says, so real feeds' noise is not an error;
structurally malformed `cc_data` fails the import like any malformed input.

Public API: none beyond the new text tracks in an import's catalog. Wire:
none.

## Closes

- [#9442](https://github.com/moq-dev/moq/issues/9442) - a player wants to draw a feed's in-band A/53 captions

## Related

- [Re-insert captions into exported video](/quest/m3/transcribe/caption-export.md) - the reverse, for containers that carry captions in band
