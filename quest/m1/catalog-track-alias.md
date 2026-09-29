# [M] hang catalog rendition keys are aliases

## Goal

A hang catalog's video and audio rendition keys are aliases, unique within
the catalog, rather than wire track names. A rendition config gains an
optional `track`, the wire track name, beside its existing `broadcast`;
absent, the track name is the alias, so every catalog published today parses
and plays unchanged. One catalog can then list renditions from several
broadcasts that share a track name.

## Plan

Decided (2026-09-29) while planning [media stats](/quest/m1/stats/README.md),
which keys its snapshots by this alias: without it, a catalog referencing two
broadcasts that both publish `video` cannot list both, and a snapshot keyed by
track name would collapse them.

- The field is `track`, beside `broadcast` on `VideoConfig` and `AudioConfig`
  in `rs/hang` and their zod schemas in `js/hang`. Additive on main: an
  optional field on `#[non_exhaustive]` types.
- Every reader that subscribes by the map key resolves the wire name through
  one helper instead, in Rust and JS, so no path keeps assuming key equals
  name. Publishers keep writing no `track` unless they need it.
- Open: a released reader ignores `track` and subscribes by the key, so it
  cannot play a rendition whose `track` differs from its alias. Candidates:
  accept that, since such a listing was not expressible before and a
  publisher sets `track` only when it must; or move the reader change to
  `dev` behind a catalog version. The maintainer settled it as additive on
  main; confirm the forward-compatibility cost before starting.
- The MSF conversion in `rs/moq-mux` names MSF tracks by the key today.
  It carries the wire name and keeps the alias through a round-trip test, or
  refuses a catalog whose `track` differs from its alias.
- Scope: `rs/hang`, `js/hang`, their readers, `drafts/draft-lcurley-moq-hang.md`
  (validate with `just drafts check`), and `doc/concept/hang.md`.
- Tests: an old catalog resolves each track name to its key; a catalog with
  two renditions naming the same `track` in different broadcasts round-trips
  in both languages and plays each.
- Open: text, JSON, and binary sections also carry `broadcast` and key by
  track name. Candidates: keep this to video and audio, as decided, or give
  every section with `broadcast` a `track` through the same helper so
  resolution is uniform.
- Open: video and audio are separate maps, so one key can appear in both.
  Within one broadcast that already means one track for two kinds, but with
  `broadcast` references it parses today. Candidates: refuse a cross-kind
  duplicate (a validation tightening on main), or accept it and kind-qualify
  the stats and echo keys.

## Related

- [Media stats](/quest/m1/stats/README.md) - keys stats and feedback by this
  alias
