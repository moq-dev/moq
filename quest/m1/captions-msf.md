# [M] MSF caption roles

## Goal

An MSF catalog's caption and subtitle tracks survive conversion into a hang
catalog. Today they are dropped with a warning, so a
round trip through MSF silently loses every caption.

## Plan

`from_msf` skips any track "with no role, with an unsupported role (caption,
subtitle, sign language, audio description, custom roles)". Now that the hang
`text` section exists with a `TextRole` borrowing the MSF role names, two of
those map straight across and stop being unsupported: `TextRole` has only
`Subtitle`, `Caption`, and `Unknown(String)`, which preserves any other role
verbatim.

Map the roles, and derive the rest of `TextConfig` from what MSF carries:
language onto `lang`, the display name onto `label`, and the packaging onto
`container` the way video and audio already do. Pick the cue `format` from the
track's codec rather than defaulting blindly, since an MSF caption track can
carry more than WebVTT and guessing wrong produces a track a player renders as
garbage.

Prove the round trip in both directions, so an MSF catalog converted to hang
and back keeps its caption tracks, roles, and languages. Audio description is
deliberately left unmapped: it is an audio rendition with a role, not timed
text, and mapping it into `text` would be wrong. Sign language is likewise a
video rendition, so it stays unmapped too. A custom role maps to
`TextRole::Unknown` only when the track's codec is a text format.

Per cross-package sync, mirror any schema movement in `js/msf`.

### Validation remaining

The conversion implementation and both review fixes are complete in [#5226](https://github.com/moq-dev/moq/pull/5226): full checks, draft validation, failing-before regressions and real browser playback pass. Keep this quest until required interop is green. The current all-language run fails only Python to native Bun: an accepted catalog subscription sees a restart announcement and is revoked, then reports `broadcast is closed`. All other matrix arms, refusals, exact browser close and finite tails pass. The cause is unproven; #5294 has no confirmed matching reproduction. Do not import it speculatively or retry away the failure.

## Related

- [Caption import](/quest/m1/captions-import.md) - the container half of the
  same gap
