# [M] Bindings report the catalog entries they skipped

## Goal

A native player built on a binding can tell its user that the catalog
listed renditions it skipped because a newer moq-hang draft defines them,
the way `@moq/watch` warns and `hang::Catalog::skipped` reports. `MoqCatalog`
in `rs/moq-ffi/src/media.rs` carries the skipped video and audio rendition
IDs with their versions, and every wrapper (Python, Go, Kotlin, Swift, Dart,
C, C++) exposes them under the same name.

## Plan

Today a binding gets a `MoqCatalog` built from `hang::Catalog`, so a
rendition a newer draft defines just vanishes: the root
`MoqError::UnsupportedCatalog` covers a reshaped root only.

Recommendations:

- One field, `skipped`, holding the video and audio maps of ID to
  `version` (the only sections `MoqCatalog` exposes), plus the highest
  version, mirroring `hang::catalog::Skipped`. Data tracks and includes
  join it when `MoqCatalog` exposes those sections.
- `moq play` and the OBS source log a warning naming the version, as
  `@moq/watch` does.

Tests: the ffi test that a catalog with a compatibility rendition and a
version-5 rendition yields the compatibility one plus the report, and each
wrapper's smoke test reads the field.

Public API: additive in moq-ffi and every wrapper. Wire: none.
