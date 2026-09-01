# [M] MSF caption roles

## Goal

An MSF catalog's caption, subtitle, and sign-language tracks survive
conversion into a hang catalog. Today they are dropped with a warning, so a
round trip through MSF silently loses every caption.

## Plan

`from_msf` skips any track "with no role, with an unsupported role (caption,
subtitle, sign language, audio description, custom roles)". Now that the hang
`text` section exists with a `TextRole` deliberately mirroring the MSF role
registry, three of those map straight across and stop being unsupported.

Map the roles, and derive the rest of `TextConfig` from what MSF carries:
language onto `lang`, the display name onto `label`, and the packaging onto
`container` the way video and audio already do. Pick the cue `format` from the
track's codec rather than defaulting blindly, since an MSF caption track can
carry more than WebVTT and guessing wrong produces a track a player renders as
garbage.

Prove the round trip in both directions, so an MSF catalog converted to hang
and back keeps its caption tracks, roles, and languages. Audio description is
deliberately left unmapped: it is an audio rendition with a role, not timed
text, and mapping it into `text` would be wrong.

Per cross-package sync, mirror any schema movement in `js/msf`.

## Related

- [Caption import](/quest/m2/captions-import.md) - the container half of the
  same gap, and the quest that closes the issue
