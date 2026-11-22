# [XS] Mark moq-net returned event and item types non_exhaustive

## Goal

Public event and item enums/structs returned by `moq-net` are marked
`#[non_exhaustive]` so adding variants or fields after `0.22.0` is not a
semver-breaking change.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fence these `moq-net` public event/item types in `m2`
before `0.22.0` freezes the API (rejected: skipping the API freeze cleanups).

Add `#[non_exhaustive]` to:

- `broadcast::Kind`, `broadcast::Listed`, and `broadcast::TracksEvent`
  (including its `Added` and `Removed` struct variants) in
  `rs/moq-net/src/model/listing.rs:31,40,74`.
- `origin::RouteEvent`, `origin::RouteKind`, and `announce::Announce` in
  `rs/moq-net/src/model/origin.rs:2249,2258,2289`.
- `announce::Event` in `rs/moq-net/src/model/announced.rs:10`.
- `track::Item` (and its `Item::Gap` struct variant) in
  `rs/moq-net/src/model/track.rs:12492`.

Update downstream pattern matches in the workspace (`rs/moq-*`) to include
wildcard arms / `..` where needed. Check whether any caller constructs these
types directly and ensure constructors or helpers exist if external callers
need them.

Public API: breaking in `moq-net` (adds `#[non_exhaustive]`). Wire: none. Run
workspace tests and API-diff checks.
