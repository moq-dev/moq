# [M] One timeline for every source on a catalog

## Goal

Every source sharing a catalog (captures, and any number of container
importers) lands on one forward timeline, whichever starts first. Today the
first importer's first frame re-anchors the catalog clock
(`catalog::Producer::anchor`). A capture that started earlier has copied the
old clock, and a second importer's anchor is a no-op, so it publishes its own
PTS base on a clock it didn't place. Either way the sources drift apart. Following the new clock doesn't help: sources near PTS 0 step it
backwards, and `container::Producer::write` refuses a group below the last one
(`TimestampRewind`), even across a discontinuity.

## Plan

Follow-up of [#4668](https://github.com/moq-dev/moq/pull/4668), which moved
data tracks onto the anchored clock. The anchor exists only on `dev`, so this
targets `dev`.

Decided (2026-10-01):

- The clock never moves once something stamps with it. The public
  `catalog::Producer::clock()` fixes the mapping (sets `anchored`). Crate-internal
  readers, like #4668's `Listing`, read the state without fixing it.
- An importer anchors once, on its first frame, and gets back an offset: zero
  if it placed the mapping (PTS stays verbatim), `clock.now() - first_pts`
  otherwise. Every frame from that importer, on all of its tracks, is shifted
  by that offset, so its audio and video stay in sync. Separate importers get
  separate offsets.
- Captures read the clock once and never re-check it. Audio already stamps by
  sample count from one `clock.now()` per epoch; there is no per-frame clock
  read to add.
- Rejected: captures re-read the clock per frame and mark a break when it
  changes. A backward anchor would hit `TimestampRewind`, and every audio
  buffer would take the catalog lock to watch for an event that should never
  happen while it is live.
- The offset rewrites only the moq-lite frame timestamp, never the payload. An
  fMP4 passthrough fragment keeps its source `tfdt`, so the two disagree. The
  frame timestamp is the broadcast timeline; a payload's timestamps are only
  relative within the frame. Ad insertion (splicing sources with unrelated PTS
  bases) needs the same rule.
- m2: nothing in-tree reaches it. `moq-cli publish` runs a capture or a stdin
  import, never both, and moq-ffi does not expose capture.

Guidance:

- The offset is signed: a stream starting at 3600 s on a clock reading 10 s
  shifts down. Refuse a frame that would land below zero rather than clamp it.
- A `with_clock` catalog is fixed from the start, so its importers offset too.
  Today they publish verbatim PTS on a clock they didn't place.
- Document on `catalog::Producer::clock` that taking the clock fixes it.
- Test: start a synthetic capture, then import an fMP4 starting at PTS 0. Both
  tracks advance from the capture's timeline with no rewind. Also cover the
  reverse order: an importer first keeps its PTS verbatim. A passthrough
  fragment whose `tfdt` disagrees with its frame timestamp decodes at the frame
  timestamp, in Rust and JS.

Public API: no new items. `catalog::Producer::clock()` now fixes the mapping,
and importers no longer publish verbatim PTS when the clock was already taken.
Wire: none.

## Required

- [CMAF frame timestamp](/quest/m2/cmaf-frame-timestamp.md) - decoders honour an offset frame timestamp on passthrough tracks

## Related

- [Audio capture time](/quest/m2/audio-capture-time.md) - maps audio's capture timeline onto the broadcast clock once per open
