# [S] hang-04 says which readers honor an entry version, and leaves overrun durations to the edge

## Goal

The hang-04 draft and its migration notes stop overpromising. An entry's
`version` gates only consumers of moq-hang-04 or later. A hang-03 reader reads
every entry and subscribes to its ID, so while hang-03 players matter a
publisher keeps a rendition's `track` equal to its ID. The HLS and DASH section
keeps the segment grid on `sync.interval` and the gap numbering, which every
edge must share. It stops prescribing how an edge lists an overrun group's
duration: that recipe moves to moq-hls's docs as gateway policy.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of a pre-cut audit of the lite-07
and hang-04 wire changes): ✅ fix both (rejected: the version text only;
moving the whole HLS and DASH section out of the draft, since moq.pro runs
relays of two versions side by side and their edges must number segments
alike).

Entry version:

- `release`'s hang-03 draft has no catalog version rule, so a hang-03 reader
  ignores `version` and `track`. The audit ran the released `@moq/hang` 0.5.4
  parser on `hd: {codec: "vp8", track: "hd-v2", version: 4}`: it kept a
  playable `hd` and dropped both fields, so a player selecting it subscribes
  to `hd`.
- In `drafts/draft-lcurley-moq-hang.md`, the Version section's "so an older
  consumer still plays the rest of the catalog, including a compatibility
  rendition beside the new one" holds only for consumers of -04 or later. Track
  Identity's "A rendition MAY keep its ID and name the new track with `track`"
  misleads a hang-03 player while one is in use.
- Put the rule in the -04 migration note too (`rs/hang/MIGRATE.md`,
  `js/hang/MIGRATE.md`, or the PR's Impact section, following RELEASE.md).

HLS overruns:

- The draft's HLS section says "A group longer than the segment is listed at
  its first line, so its duration understates its media, and the gaps after
  it overstate theirs." Keep the grid, the numbering, and that an edge
  tolerates an overrun. Move how its durations are listed to `doc/bin/hls.md`,
  beside `rs/moq-hls/src/export/grid.rs`'s module docs, which already describe
  it.
- There is no accurate alternative to build: an EXTINF above
  EXT-X-TARGETDURATION is forbidden (RFC 8216 section 4.3.3.1), and the target
  duration can't change within a playlist (section 6.2.1).

Wire: none. Catalog JSON: none; the draft's text and the migration notes.
