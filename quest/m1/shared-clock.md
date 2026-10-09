# [L] One timeline for every source on a catalog

## Goal

Every source sharing a catalog (captures, and any number of container
importers) lands on one forward timeline, whichever starts first. Today the
first importer's first frame re-anchors the catalog clock
(`catalog::Producer::anchor`). A capture that started earlier has copied the
old clock, and a second importer's anchor is a no-op, so it publishes its own
PTS base on a clock it didn't place. Either way the sources drift apart. Following the new clock doesn't help: sources near PTS 0 step it
backwards, and `container::Producer::write` refuses a group below the last one
(`TimestampRewind`), even across a discontinuity.

The catalog's root `clock` is also final from the first snapshot a consumer
sees. A lone importer already holds its catalog until its first frame
anchors; a container set up after a data track or catalog section has
published (any order moq-c and moq-ffi allow) still re-anchors it after
copy-once readers (moq-hls export, derived broadcasts) took the old one.

## Plan

Follow-up of [#4668](https://github.com/moq-dev/moq/pull/4668), which moved
data tracks onto the anchored clock. The anchor is unreleased.

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

Decided (2026-10-05):

- The first catalog publish also fixes the clock, whatever triggered it: a
  snapshot on the wire is a mapping a reader may have copied. A data track or
  section publishes the moment it is registered (`Producer::data_entry` holds
  its reservation only for `init`), so a container set up after one gets an
  offset. This lands with the offset, not before it, so the rule never exists
  without it.
- m1: the anchor is unreleased, and landing this before the cut means no
  release ships a clock that moves after a publish.
- Rejected for a container joining a published clock: a no-op with verbatim
  PTS (an fMP4 an hour into its `tfdt`, or a TS PTS up to about 26.5 h, lands
  that far off), a warning or refusal (a regression for an order the bindings
  allow), and holding every publish until a container anchors (a data-only or
  capture-only catalog would never publish). Fixing the clock at catalog
  creation is rejected too: even a lone importer would carry frame timestamps
  that disagree with its `tfdt`.
- One source's offset lives on a public `catalog::Input` handle, separate
  from the publication gate: `catalog.input()` returns a clonable handle for
  one PTS base, and `input.reserve()` mints a `Reserved` gated as today that
  shares the input's offset. The first anchor through any of them sets it.
  `catalog.reserve()` keeps a fresh offset per call. Holding an `Input` never
  withholds the catalog. moq-hls import holds one for the whole import, so its
  renditions (separate fMP4 importers on one PTS base) and every replacement
  importer on an `EXT-X-MAP` change share one offset; separate offsets would
  shift each by its first frame's arrival gap. Named `Input`, not `Source`,
  which would clash with the public `moq_mux::Source`; `Timebase` was the
  other candidate.
- Rejected: sharing the offset through `Reserved` clones. A live `Reserved`
  withholds the initial catalog, so a handle kept for later importers would
  hold it for the whole import. Also rejected: importers handing the offset
  to each other (`with_offset` on all four).
- A `with_clock` catalog is fixed from the start, so every importer gets an
  offset, the first included. Otherwise a later importer could end up with a
  negative timestamp. This drops the promise that a `with_clock` PTS zero
  names a recording's real start. Rejected: the first importer keeps its PTS
  and only later ones shift, and no `with_clock` importer shifts.
- One payload exception: an SCTE-35 section on a section-framed verbatim track
  absorbs the offset in its `pts_adjustment` (modulo 2^33, at 90 kHz), with
  `CRC_32` recomputed. Its `pts_time` sits on the source PTS base, and nothing
  downstream can recover the offset: the section's frame timestamp is the video
  clock at arrival plus that same offset. The field keeps its size and is clear
  even in an encrypted section, so TS export and the typed cues of
  [#2279](/quest/m3/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md)
  read splice times on the broadcast timeline with no change of their own.
- Rejected for SCTE-35: recording the offset in the `mpegts` catalog section for
  TS export to add (a new catalog field every typed consumer would also have to
  apply), refusing to offset a TS import that carries sections (SCTE-35 could
  never share a clock), and deferring to #2279 (this quest is what makes the
  splice times stale).

Decided in the 2026-10-06 audit (with
[Same-epoch importers](/quest/m1/hop-aligned-import.md)):

- A same-epoch (redundant) importer's offset or anchor comes from its input
  (its PTS or PCR) or from the shared epoch, never from `clock.now()`. It
  supplies that input-derived anchor through this quest's `Input`/offset API,
  so Same-epoch importers only provides it, and two importers of one stream
  publish identical timestamps even when the clock is already fixed. The
  arrival-derived `clock.now() - first_pts` above stays the rule for every
  other importer. Rejected: keeping the two quests' anchoring separate.

Guidance:

- The offset is signed: a stream starting at 3600 s on a clock reading 10 s
  shifts down. Refuse a frame that would land below zero rather than clamp it.
- Docs: document on `catalog::Producer::clock` that taking the clock fixes
  it. Fix every doc that says the clock re-anchors or that `with_clock` keeps
  PTS verbatim: `Config::with_clock` in `catalog/producer.rs`,
  `doc/lib/rs/moq-mux.md` (the importer paragraph: a data track created before
  the first frame following the anchor, and a `with_clock` zero naming the
  real start), `doc/setup/upgrade.md` (pinning with `with_clock` when the
  source's zero is known, and reading `catalog.clock()` at write time because
  an importer re-anchors it), and the `binary.rs` module doc's "read at write
  time" note. Replace `with_clock_names_the_contents_real_start` with a test
  that a `with_clock` catalog's first importer is shifted onto it.
- Test: a data track registered before a container's first frame, then a
  container starting at a nonzero PTS: the clock never moves, and the
  container's frames land at now. This rewrites
  `a_write_follows_the_anchored_clock` (`binary.rs`, `json.rs`) and
  `an_anchor_is_not_jitter` (`binary.rs`), which create a data track and then
  anchor.
- Test: moq-hls import receives its initial catalog while its `Input` is
  alive, then replaces a rendition's importer, and the replacement keeps the
  same offset.
- Test: a TS import carrying a `splice_insert` joins a clock already in use. In
  its TS export, `pts_time + pts_adjustment` lands on the exported video PTS of
  the splice point, and the section's CRC verifies.
- Test: the first data-track test also runs with a synthetic capture taking
  the clock in place of the data track, importing an fMP4 at PTS 0: both
  tracks advance from the capture's timeline with no rewind. Also cover the
  reverse order: an importer first on a default clock keeps its PTS verbatim.

Public API: `catalog::Input`, `catalog::Producer::input`, and
`Input::reserve` are new, and `Input` accepts an input-derived anchor in
place of the arrival one. `catalog::Producer::clock()` and the first publish
now fix the mapping, and importers no longer publish verbatim PTS when the
clock was already taken or set with `Config::with_clock`.
Wire: none.

## Related

- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - supplies the input-derived anchor through this quest's API
