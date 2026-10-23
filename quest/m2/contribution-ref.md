# [M] Hang contribution references

## Goal

A Hang root catalog can name a whole contribution broadcast, relative to its
own path, without opening it: each guest feed in a composed room, for
example. Rust and JavaScript catalogs carry the entry, and `@moq/watch`
resolves one only when a consumer selects one of its renditions. This backs
moq.pro's overlay contribution reference,
whose composer never reads a contribution's catalog because subscribing is
the demand that starts the encoder.

## Plan

Decided 2026-10-20 (maintainer): an upstream quest backs this work, because
#5722 and #5298 closed unmerged for lack of one. Only the catalog-level
reference: the processor media contract that once carried it (#4946) stays
deleted.

Salvage `refs/pull/5722/head` ("feat(hang): add whole contribution
references") and its decision notes rather than starting over:

- A root `contributions` map, `{ [id]: { broadcast, epoch? } }`. `broadcast`
  is a relative reference with the usual escape check, like the
  per-rendition `broadcast` reference in `rs/hang/src/catalog/video/mod.rs`
  and its audio, text, JSON, and binary counterparts.
- Listing an entry subscribes to nothing. `@moq/watch` opens one only
  through `Broadcast.contribution(effect, id)`, a child broadcast scoped to
  the caller's effect, resolved through the parent's `relativeBroadcast`.
  The child's rendition IDs stay its own, so a contribution's rendition
  never shadows a source rendition with the same ID.
- `epoch` follows the reference epochs that landed in
  [#5699](https://github.com/moq-dev/moq/pull/5699): the same validation, known-mismatch refusal, and epochless best effort. A
  lazy contribution has no anticipated epoch, so a composer leaves it out
  until the contribution resolves. A reader that follows announcements
  follows the contribution's Start, End, and Restart; one that follows none
  pins the first resolution for its parent catalog instance. Reuse
  `moq_mux::Source`'s pinning and `Source::follow`; do not add a second
  one.
- #5722 was stacked on #5699's reference `epoch`, which has landed; rebuild
  it on `main`.

Per cross-package sync, land `rs/hang`, `js/hang`, `js/watch`, and
`drafts/draft-lcurley-moq-hang.md` in one PR. Cover lazy resolution (listing
the entry opens nothing), a missing contribution, a malformed entry refused,
a relative path that escapes, an ID collision with a source rendition, and a
contributor restart.

Public API: a catalog `contributions` section in `hang` and `@moq/hang`, and
`Broadcast.contribution` in `@moq/watch`. Wire: a new optional catalog
field.
