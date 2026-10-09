# [M] hang catalog rendition keys are IDs

## Goal

A hang catalog's video and audio rendition keys are IDs, unique across video
and audio within the catalog, rather than wire track names. A rendition
config gains an optional `track`, the wire track name, beside its existing
`broadcast`; absent, the track name is the ID, so every catalog published
today parses and plays unchanged. One catalog can then list renditions from
several broadcasts that share a track name. A catalog that repeats an ID
across video and audio is refused.

## Plan

Decided (2026-09-29) while planning [media stats](/quest/m1/stats/README.md),
which keys its snapshots by this ID: without it, a catalog referencing two
broadcasts that both publish `video` cannot list both, and a snapshot keyed by
track name would collapse them.

Decided by the maintainer in the 2026-10-06 audit: "make the key be the ID,
adding in a dedicated `track` field". The key is an ID (this quest earlier
called it an alias), unique across video and audio; a cross-kind duplicate is
refused, a validation tightening on main, so stats and echo key by ID alone
with no kind qualifier; `track` stays optional and defaults to the ID.
Rejected: an ID plus a required `track`.

- The field is `track`, beside `broadcast` on `VideoConfig` and `AudioConfig`
  in `rs/hang` and their zod schemas in `js/hang`. Additive on main: an
  optional field on `#[non_exhaustive]` types.
- Every reader that subscribes by the map key resolves the wire name through
  one helper instead, in Rust and JS, so no path keeps assuming key equals
  name. Publishers keep writing no `track` unless they need it.
- Video and audio are separate maps, so one key could appear in both; with
  `broadcast` references it parses today. Parsing refuses it in Rust and JS.
- Open: a released reader ignores `track` and subscribes by the key, so it
  cannot play a rendition whose `track` differs from its ID. Candidates:
  accept that, since such a listing was not expressible before and a
  publisher sets `track` only when it must; or gate the reader change
  behind a catalog version. The maintainer settled it as additive on
  main; confirm the forward-compatibility cost before starting.
- The MSF conversion in `rs/moq-mux` names MSF tracks by the key today.
  It carries the wire name and keeps the ID through a round-trip test, or
  refuses a catalog whose `track` differs from its ID.
- Scope: `rs/hang`, `js/hang`, their readers, `drafts/draft-lcurley-moq-hang.md`
  (validate with `just drafts check`), and `doc/concept/hang.md`.
- Tests: an old catalog resolves each track name to its key; a catalog with
  two renditions naming the same `track` in different broadcasts round-trips
  in both languages and plays each; a catalog repeating an ID across video
  and audio is refused.
- Open: text, JSON, and binary sections also carry `broadcast` and key by
  track name. Candidates: keep this to video and audio, as decided, or give
  every section with `broadcast` a `track` through the same helper so
  resolution is uniform.

## Related

- [Media stats](/quest/m1/stats/README.md) - keys stats and feedback by this
  ID
