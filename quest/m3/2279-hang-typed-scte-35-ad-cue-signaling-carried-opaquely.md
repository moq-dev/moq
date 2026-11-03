# [L] hang: SCTE-35 cues a player can act on

## Goal

An ad cue carried through MoQ is readable by a browser without knowing MPEG-TS
exists, and a player can raise an event on it. Today `splice_info_section`
bytes ride byte-faithfully on a TS-specific verbatim track that only the TS
exporter understands. Server-side ad insertion is a separate future quest.

## Plan

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer for timed metadata.

Use the shared event contract [emsg](/quest/m3/emsg.md) settles. Deliver cues immediately, including when splice_time is in the
future; consumers need advance notification. Metadata group sequences are
independent of media GOPs.

- A top-level `scte35` catalog section names cue tracks with optional program
  or rendition association. Video is not required for audio-only programs.
- Preserve complete raw splice_info_section bytes. Carry the event's
  presentation time on the broadcast clock; commands without splice_time
  use the media clock observed at arrival. Do not delay publication until the
  target media group opens or use its sequence as the event identity.
- Preserve source timing and placement needed by TS export separately from
  the event's presentation time. The shared contract settles the outer
  timestamp and retention behavior for a future cue followed by a current event.
- **Typed decode is a helper, not the wire.** A parser for `splice_insert`,
  `time_signal`, and segmentation descriptors (a maintained crate if one is
  adequate) lives beside the section in Rust and JS, and `js/watch` raises a
  cue event from it. Applications that want the rest parse the bytes.
- **The TS lanes stay.** The importer keeps the verbatim `mpegts` track for
  contribution fidelity and additionally emits the sidecar; the exporter must
  not double-emit: an import-to-export test with the sidecar present asserts
  each source section is emitted exactly once, including audio-only programs.

Wire and catalog schema: a new optional section, additive, so it lands on
`main`, with `drafts/draft-lcurley-moq-hang.md` updated in the same PR as the
schema: the draft is the normative spec, and a section Rust and JS emit must
be in it the day it ships. Cross-package sync: `rs/hang`, `js/hang`,
`rs/moq-mux`, `doc/concept`.

## Required

- [fMP4 emsg](/quest/m3/emsg.md) - settles the shared timed-metadata contract this builds on, which needs maintainer agreement first

## Related

- [ID3 catalog section](/quest/m3/id3.md) - the other typed timed-metadata section, same rule
