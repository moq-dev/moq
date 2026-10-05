# [S] Importers publish the catalog at their first frame

## Goal

A catalog fed by one container importer is first published with its final
root `clock`. Today fMP4 and MKV imports publish the catalog when their init
segment (`moov`, `Tracks`) resolves the reservation, and an Opus-only MPEG-TS
import when its PMT does, on a provisional clock, then re-anchor it on the
first frame (`catalog::Producer::anchor`). Readers that copy the clock once
(moq-hls export's `EXT-X-PROGRAM-DATE-TIME` and `availabilityStartTime`,
derived broadcasts) keep the provisional one, and the hang draft says the
mapping is "fixed for the broadcast".

Non-goal: a clock that never moves after any publish. A container set up
after a data track or catalog section has published still re-anchors once;
[shared-clock](/quest/m1/shared-clock.md) makes the first publish fix the
clock and offsets that container instead.

## Plan

The anchor is unreleased, and this breaks an in-tree path (an fMP4 or MKV
import read by moq-hls export), so it lands before the next release cut.
Requested by an external consumer (OneTooMany). Ranked first in m1, ahead of
shared-clock and cmaf-frame-timestamp (maintainer, 2026-10-05).

Decided (2026-10-05):

- fMP4, MKV, and MPEG-TS hold their initial reservation until their first
  frame anchors, as FLV already does (`flv/import.rs`, "anchored before the
  reservation below publishes"). The first snapshot then carries the anchored
  clock. PTS stays verbatim for a single importer, so a passthrough fragment's
  `tfdt` still agrees with its frame timestamp.
- Split from the strict "first publish fixes the clock" rule, which needs
  shared-clock's offset for a container joining a fixed clock. The hold alone
  needs no offset and regresses nothing: an earlier publish still lets the
  anchor move the clock, as on `main`.
- Rejected: omitting `clock` until anchored. Adding it later is still a change
  copy-once readers miss.

Guidance:

- An fMP4 `moov` declares every track at once, and MKV's `Tracks` likewise, so
  the first frame of any track releases the hold.
- MPEG-TS drops its hold after the PMT (`ts/import.rs`, "Every stream in the
  initial program is registered now"). Video configs resolve only on the first
  frame, after the anchor, but Opus builds its config from PMT descriptors, so
  an Opus-only stream publishes before its first PES anchors. Keep the hold
  until the first anchor.
- fMP4 detects a duplicate `moov` by its reservation being gone
  (`fmp4/import.rs`, `DuplicateMoov`). Holding it to the first fragment lets a
  second `moov` slip through, so key the check on the parsed `moov` instead.
  Likewise check that a TS PMT version change before the first PES does not
  re-reserve against the held handle.
- Tests (fail on `main` for fMP4, MKV, and Opus-only TS): for each of fMP4,
  MKV, FLV, and TS, a catalog consumer's first snapshot carries the anchored
  clock and later snapshots never change it. The TS case is Opus-only with a
  nonzero first PTS, since a video stream's config resolves only on its first
  frame, after the anchor. Also a TS program of only verbatim PIDs, which
  publish through `modify` with no reservation. Plus an end-to-end fMP4 import
  into moq-hls export whose `EXT-X-PROGRAM-DATE-TIME` matches the anchored
  clock.
- Docs: fix the `Config::with_clock` and `Producer::anchor` comments and
  `doc/lib/rs/moq-mux.md`, and add an `Unreleased` line in
  `doc/setup/upgrade.md`: the catalog now appears at an importer's first
  frame, not its init segment.

Public API: no new items. A catalog fed by an fMP4, MKV, or Opus-only TS
importer is published at the first frame instead of the init segment. Wire:
none; the hang draft already says the mapping is fixed.
