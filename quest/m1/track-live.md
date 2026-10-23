# [M] A reader can tell when its track is live

## Goal

A track reader in moq-net and `@moq/net` can wait for, and check, whether its
track's feed is live: a local producer from the start, a relay's copy once its
upstream subscription answers. A player uses it to skip the old cached groups
a warm copy serves while it waits for a fresh upstream subscription, instead
of rendering them.

## Plan

`largest()` already waits for live internally (#5924), so it never reports
what an ended feed left; this quest makes the signal itself public.

- Rust has the state: `TrackState::feed` (`Live`, `Idle`, `Answered`) and
  `readable()` in `rs/moq-net/src/model/track.rs`, the crate-private
  `resume` `poll_live`, and the crate-private `Producer::is_live`. Expose one
  reader-side `poll_live`/`live()` (or `is_live`) on `track::Subscriber` and
  `track::Ordered`, and decide whether the publishers' crate-private
  `poll_live` folds into it (see
  [Public serving](/quest/m1/public-model/serve.md)).
- JS has no equivalent: a track never goes idle, so its reader is live from
  the start. Mirror the name, and the idle state if JS gains warm copies.
- Decide what a reader does with groups served before live: keep serving
  them (today) and let the caller skip by the signal, or offer a mode that
  starts at the live edge.

## Related

- [Public serving](/quest/m1/public-model/serve.md) - the publishers' `poll_live` and the `largest()` decision
