# [S] An export's later requests stay on the broadcast it resolved

## Goal

`moq_mux::Source` keeps the first `broadcast::Consumer` it resolves for each
path, its own and every cross-broadcast reference, and serves every later
request for that path from that handle. A late rendition subscribe or SI
repoint never lands on a replacement, epoch or not. On a replaced instance
it fails loudly instead of splicing the new publisher's data into the old
program.

## Plan

Found by Codex on #5147 (2026-10-10), planned the same day. `Source` pins
its own path by epoch (`epoch` in `rs/moq-mux/src/source.rs`, set through
`pinned` from `ts::Export`). An epochless route (every default lite-06
session) has `None`, so each later request re-resolves the path, and if a
replacement won in the meantime, it serves that request even without
`--stitch`.

- Decided: pin by handle, replacing the epoch field. One mechanism covers
  epoch and epochless routes. A replaced instance's sticky front refuses new
  tracks, so a late request on it errors, and the Follower decides what
  happens next (`Replaced`, or a stitch).
- Decided: pin every resolved path, not only the source's own, so a replaced
  sibling (`broadcast: ./source`) cannot splice in either.
- `ts::Follower` / `Export::follow` build a new `Source` for the broadcast
  they follow, so a stitch starts with an empty pin map.
- Tests, with mocked time, on an epochless route: an export resolves `live`,
  a replacement wins `live`, and then a rendition added to the old catalog
  (and an SI repoint) fails rather than reading the replacement. The same
  for a sibling reference. Today's epoch-pin test (`source.rs`) moves to
  the handle.

Public API: none (`pinned` is crate-private). Wire: none.
