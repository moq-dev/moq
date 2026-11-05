# Content protection

## Goal

Common Encryption CMAF (`cenc` and `cbcs`) publishes over MoQ unchanged.
The hang catalog's `drm` section and the MSF catalog's CMSF `contentProtections` (draft-ietf-moq-cmsf section 4) signal each protected rendition's scheme, keys, DRM systems, and license servers.
Rust, the CLI, and every binding can set the license servers.
Key rotation, clear lead, and DRM for LOC tracks are out of scope.

## Plan

Imported from [#7766](https://github.com/moq-dev/moq/issues/7766) on 2026-11-03, whose reporter has a working prototype ([michaelstuurman/moq@fcf90470d](https://github.com/michaelstuurman/moq/commit/fcf90470d91bd3a868aea20448cf3e5bc4b54978)) that plays in Chrome with ClearKey.
Decided with the maintainer on 2026-11-03:

- The hang catalog gets a root `drm` section, and the MSF catalog renders it into CMSF's fields, since MSF is always rendered from hang. A license URL needs a home that the hang catalog also carries.
- mp4-atom gains the protection boxes upstream, rather than moq-mux splicing a raw sample entry back into the init it encodes.
- `<moq-watch>` hides protected renditions as unplayable. WebCodecs cannot decrypt, and browser playback through MSE and EME is [deferred](/quest/m3/drm-playback.md).

## Required

- [mp4-atom protection boxes](/quest/m2/drm/mp4-atom.md) - mp4-atom round-trips `encv`/`enca`, `sinf`, and `pssh`, released and bumped here
- [Protected CMAF import](/quest/m2/drm/import.md) - `moq publish import fmp4 --cmaf` passes CENC media through, signalled in hang `drm` and MSF `contentProtections`, with license URLs from Rust and the CLI
- [Protected CMAF in the bindings](/quest/m2/drm/bindings.md) - moq-ffi and every wrapper set license URLs on import and read each rendition's `drm` entries
