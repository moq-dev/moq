# [XS] Percent-encode DASH rendition URLs

## Goal

The MPD's `initialization` and `media` URLs carry each rendition name as one
percent-encoded path segment, as the HLS master playlist already does, so a
name containing `/`, `?`, `#`, `%`, or `$` resolves to its own rendition. Today
`video/1080p` under broadcast `live` routes to broadcast `live/video`,
rendition `1080p`.

## Plan

Found by the final-head audit of
[#4034](https://github.com/moq-dev/moq/pull/4034); the bug predates it.
Decided 2026-10-07: m2, since first-party publishers name renditions like
`video0`, though the catalog allows slashes.

In `rs/moq-hls/src/export/mpd.rs` `render_representation`, encode the name
with the HLS renderer's `PATH_SEGMENT` set (`rs/moq-hls/src/export/master.rs`),
shared rather than copied, then XML-escape the finished URL. The
`Representation` `id` stays as is. Regress the names above through rendering
and `Route::parse` in `rs/moq-hls/src/server/routes.rs`.
