# [M] An export's later requests stay on the broadcast it resolved

## Goal

`moq_mux::Source` is built from the catalog broadcast its caller resolved,
keeps the first `broadcast::Consumer` it resolves for each cross-broadcast
reference, and serves every later request for a path from that handle. A
late rendition subscribe or SI repoint never lands on a replacement, epoch
or not. On a replaced instance it fails loudly instead of splicing the new
publisher's data into the old program.

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
- Decided (2026-10-10, amending the lazy own-path cache): `Source` is built
  from the resolved catalog broadcast, which seeds its own-path handle. Most
  callers (e.g. moq-cli, moq-ffi, moq-c, moq-hls, moq-rtc, moq-rtmp, and the
  fmp4, flv, and codec exporters) subscribe the catalog outside `Source`, so
  a lazy first resolve could still land on a replacement. Construction closes
  that for every exporter. moq-hls's own catalog hold (`export/upstream.rs`)
  should then be redundant, once `Source` returns the seeded handle for every
  self-reference; delete it if so. Callers that let the export resolve today
  (the TS export from `moq publish`) resolve first, or the export seeds the
  handle from the exact broadcast it resolved, never a second resolve; pick
  whichever reads simpler.
- The pin lifecycle follows the export's instance. A stitch builds a new
  `Source` whose own-path entry is seeded with the broadcast `follow` was
  given (sibling entries start empty), so its first track request can't
  resolve a newer replacement. A same-epoch return replaces the own-path
  handle with the returned broadcast, since the old one's routes are gone.
- A sibling's guarantee starts at its first resolution. A sibling the catalog
  first references after a replacement resolves whatever serves it then.
  The export never held another instance of that path, so that isn't a
  splice.
- Tests, with mocked time, on an epochless route: an export resolves `live`,
  a replacement wins `live`, and then a rendition added to the old catalog
  (and an SI repoint) fails rather than reading the replacement. The same
  for a sibling reference already resolved. A `Source` built from `live`
  after a replacement already won still reads the broadcast it was given on
  its first track request. A stitch whose replacement is
  itself replaced before the first track request still reads the broadcast
  it followed. A same-epoch return reads the returned handle. Today's
  epoch-pin test (`source.rs`) moves to the handle.

Public API: breaking. `Source::new` takes the resolved catalog broadcast,
and every caller in the repo moves with it (follow the Cross-Package Sync
table for moq-ffi and moq-c). A `Source` reused across a route replacement
returns its pinned handle for a path it already holds (`broadcast`,
`catalog`, and track requests), or fails on the replaced instance, instead
of resolving the current route. Document it on `Source`, fix stale
comments inline, and add a line for the `Source::new` break to the
Unreleased section of `doc/setup/upgrade.md`; no new doc page (decided
2026-10-10). Wire: none.

## Related

- [Retired requests](/quest/m0/broadcast-epoch/retired-requests.md) - held track FETCH and JS wire operations also need instance validity after retirement; handle pinning alone does not fix them
- [Catalog references](/quest/m0/broadcast-epoch/catalog-references.md) - players adopt the same sibling policy, and optional catalog epochs constrain initial resolution where supported
