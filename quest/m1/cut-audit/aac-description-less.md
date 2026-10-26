# [S] A description-less AAC rendition means one thing everywhere

## Goal

An `mp4a.40.*` catalog entry with no `description` has one defined frame
format, written in the hang draft, and every player and exporter (JS watch,
moqsrc, moq-audio, TS export) decodes it the same way.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred and
spot-verified, inherited.

Today `rs/moq-gst/src/source/imp.rs:1769` advertises `stream-format=adts`,
`js/watch/src/audio/decoder.ts:488` configures WebCodecs with no description
(which WebCodecs reads as ADTS), `rs/moq-audio/src/aac.rs:68` synthesizes an
AudioSpecificConfig and decodes the frames as raw, and
`rs/moq-mux/src/container/ts/export.rs:3823` treats them as raw and adds
ADTS headers. Whichever form a publisher sends, half the consumers break.

Decision for the maintainer, with a recommendation:

1. **ADTS (recommended).** Matches WebCodecs, the browser path most viewers
   use. moq-audio parses ADTS headers and TS export passes the frames
   through.
2. Raw access units. JS and moqsrc must synthesize a description.

State the choice in `drafts/draft-lcurley-moq-hang.md` and add a test per
consumer with a description-less fixture. Check which first-party
publishers emit description-less AAC today, so none starts producing the
other form.

Public API: none. Wire: a format clarification in the hang draft.
