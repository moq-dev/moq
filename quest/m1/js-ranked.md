# [S] JS rendition ranking

## Goal

`@moq/hang` ranks video renditions the same way as Rust's
`hang::catalog::Video::ranked` (largest picture, then highest bitrate, ties in
name order), and `@moq/watch`'s fallback pick uses it, so the browser and the
native egresses can't drift apart.

## Plan

Mirror the Rust name and semantics. `bestRendition` in `js/watch` already
matches today; replace it rather than keep two copies. Check whether the
`byDimensions`/`byBitrate` filters can reuse the same ordering.
