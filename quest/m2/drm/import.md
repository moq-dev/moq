# [L] Protected CMAF import

## Goal

`moq publish ... import fmp4 --cmaf` publishes Common Encryption fMP4 (`cenc` and `cbcs`) unchanged, and both catalogs signal the protection:

- Each protected rendition's per-track init keeps its `encv`/`enca` entry and the moov's `pssh` boxes, as CMSF section 4.2 requires.
- The hang catalog carries a root `drm` section that each protected rendition references. A dangling reference is refused, like a missing `initRef`.
- The MSF catalog renders that section as CMSF's `contentProtections` and per-track `contentProtectionRefIDs`, and parsing an MSF catalog maps them back.
- License and FairPlay certificate URLs are set from Rust and from a `moq publish` flag.
- `<moq-watch>` hides protected renditions as unplayable.

## Plan

Start from the reporter's prototype ([michaelstuurman/moq@fcf90470d](https://github.com/michaelstuurman/moq/commit/fcf90470d91bd3a868aea20448cf3e5bc4b54978)), on the typed boxes from [mp4-atom](/quest/m2/drm/mp4-atom.md) instead of its entry swap.

### Catalog

- The section is hang's own, mirroring CMSF section 4.1 entries 1:1 so rendering MSF is a straight mapping. Renaming CMSF's members to hang's style is welcome (maintainer, 2026-11-03); propose the names in the PR.
- Each entry is one DRM system, one scheme, and one default KID. A CMSF entry names a single DRM system, and listing several KIDs in one entry would lose which key a track uses, so video and audio under separate keys get separate entries.
- Scheme, default KID, system ID, and `pssh` come from each init's `schm`, `tenc`, and `pssh`. Tracks with the same system, scheme, and key share an entry.
- License and certificate URLs are not in the media, so the publisher supplies them per DRM system through the importer's Rust config and a `moq publish` flag (maintainer, 2026-11-03: Rust, CLI, and [bindings](/quest/m2/drm/bindings.md)). A URL for a system the media does not carry is refused.
- A rendition's codec string names the codec `frma` wraps (`avc1...`, `mp4a...`), as MSE and EME expect.
- Leave out CMSF's authorization URL (section 4.1.1.4.4), which the draft gives no member name. Check `robustness`'s member name against the current draft.

### Import

Passthrough only.
Passthrough re-encodes each fragment, which moves `senc`, so point the fragment's `saio` at its IVs again.
Refuse, rather than publish undecryptable media:

- encrypted input without passthrough, because unwrapping loses the per-sample IVs;
- an encrypted track whose edit list drops samples, because the kept samples' auxiliary information is not restated;
- an encrypted track without any `pssh`, because the catalog could not name its DRM system and, per CMSF section 4.1.2, would read as clear;
- CENC auxiliary information outside `senc`, or split over several `saio` boxes;
- key rotation (`pssh` in a `moof`, or `seig` sample groups);
- a clear lead (an `stsd` with both an encrypted and a clear entry), which stays refused as multiple codecs.

### Tests and docs

Fixtures: the prototype's ClearKey `cenc` file (ffmpeg's `cenc-aes-ctr` plus an added ClearKey `pssh`), and a `cbcs` file from Shaka Packager or Bento4, since ffmpeg cannot encrypt `cbcs`.
Keep both small.
Update `js/hang`'s catalog types, `drafts/draft-lcurley-moq-hang.md`, the catalog page under `doc/concept`, and `doc/bin/cli.md`.

## Required

- [mp4-atom protection boxes](/quest/m2/drm/mp4-atom.md) - the typed `encv`, `sinf`, and `pssh` atoms this builds on

## Closes

- [#7766](https://github.com/moq-dev/moq/issues/7766) - encrypted CMAF is refused and the MSF catalog can't signal CMSF content protection
