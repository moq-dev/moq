# [M] Native consumers skip disabled renditions

## Goal

Native consumers (moq-cli play, moq-gst, the moq-ffi and C convenience paths,
and audio selection generally) never select a rendition with
`enabled: false`, as `@moq/watch` does after #4915. If every supported
rendition is disabled, select none until an enabled rendition appears.

## Plan

PR #4915's `hang::catalog::Video::ranked()` orders by enabled status before
picture area and bitrate; it still yields disabled entries. Ranking alone
cannot enforce this goal when every rendition is disabled. Share eligible
rendition selection for video and audio rather than repeating checks at each
consumer. Preserve the existing quality ordering among enabled renditions.
Test mixed and all-disabled catalogs, plus disable/re-enable transitions.

A C `moq_consume_audio_enabled` getter is added only if a C consumer needs it;
any FFI/C API change updates all wrappers and their docs in the same PR.
Update native consumer docs for the selection behavior.

## Required

- [Enabled flag](/quest/m1/catalog-enabled.md) - #4915 provides enabled fields and enabled-first video ranking
