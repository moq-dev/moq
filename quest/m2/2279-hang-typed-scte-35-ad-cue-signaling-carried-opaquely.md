# [L] hang: SCTE-35 cues a player can act on

## Goal

An ad cue carried through MoQ is readable by a browser without knowing MPEG-TS
exists, and a player can raise an event on it. Today `splice_info_section`
bytes ride byte-faithfully on a TS-specific verbatim track that only the TS
exporter understands. Server-side ad insertion is a separate future quest.

## Plan

Rescoped in the 2026-09 planning pass to the sidecar rule every metadata
section follows (see [SEI section](/quest/m2/sei/sei.md)): a metadata track
belongs to one rendition, uses that rendition's group sequence, stamps each
frame with the wire timestamp of the media it applies to, and carries raw
bytes. Not a `moq-json` stream of typed records.

- **A `scte35` catalog section**, as its own `CatalogExt` like the `mpegts`
  one, naming one sidecar track per rendition that carries cues. Cues from a
  TS program attach to its video rendition, or to its audio rendition when
  there is no video; the import already stamps them with video PTS.
- **Framing.** Each frame is one complete `splice_info_section`, byte for
  byte, in the group of the media it applies to, stamped with the splice
  time's wire timestamp. Cues are sparse, so most groups are empty; a joiner
  at a keyframe gets that GOP's cues.
- **Typed decode is a helper, not the wire.** A parser for `splice_insert`,
  `time_signal`, and segmentation descriptors (a maintained crate if one is
  adequate) lives beside the section in Rust and JS, and `js/watch` raises a
  cue event from it. Applications that want the rest parse the bytes.
- **The TS lanes stay.** The importer keeps the verbatim `mpegts` track for
  contribution fidelity and additionally emits the sidecar; the exporter must
  not double-emit, and section-framed export still needs the video clock.

Wire and catalog schema: a new optional section, additive, so it lands on
`main`, with `drafts/draft-lcurley-moq-hang.md` updated in the same PR as the
schema: the draft is the normative spec, and a section Rust and JS emit must
be in it the day it ships. Cross-package sync: `rs/hang`, `js/hang`,
`rs/moq-mux`, `doc/concept`.

## Closes

- [#2279](https://github.com/moq-dev/moq/issues/2279) - close this issue when the quest finishes

## Related

- [SEI section](/quest/m2/sei/sei.md) - defines the sidecar rule this follows
- [ID3 catalog section](/quest/m2/id3.md) - the other typed timed-metadata section, same rule
