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
- The pin lifecycle follows the export's instance. A stitch builds a new
  `Source` whose own-path entry is seeded with the broadcast `follow` was
  given (sibling entries start empty), so its first track request can't
  resolve a newer replacement. A same-epoch return replaces the own-path
  handle with the returned broadcast, since the old one's routes are gone.
- The guarantee starts at a path's first resolution. A sibling the catalog
  first references after a replacement resolves whatever serves it then.
  The export never held another instance of that path, so that isn't a
  splice.
- Tests, with mocked time, on an epochless route: an export resolves `live`,
  a replacement wins `live`, and then a rendition added to the old catalog
  (and an SI repoint) fails rather than reading the replacement. The same
  for a sibling reference already resolved. A stitch whose replacement is
  itself replaced before the first track request still reads the broadcast
  it followed. A same-epoch return reads the returned handle. Today's
  epoch-pin test (`source.rs`) moves to the handle.

Public API: no signature change, but a behavior change. A `Source` reused
across a route replacement now returns its pinned handle for a path it
already resolved (`broadcast`, `catalog`, and track requests), or fails on
the replaced instance, instead of resolving the current route. Document it
on `Source`. Wire: none.
