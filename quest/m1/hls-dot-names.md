# [XS] HLS and DASH export refuse dot-only rendition names

## Goal

A rendition named `.` or `..` gets no HLS playlist or DASH representation, with
a logged error, while its sibling renditions keep serving. Today
`PATH_SEGMENT` in `rs/moq-hls/src/export/master.rs` (shared by the DASH
manifest) keeps `.`, so `..` yields `@i/video/../media.m3u8`, which a client
resolves to `@i/media.m3u8`, a path the router cannot serve.

## Plan

From the 2026-10-24 moq-hls audit. Decided with the maintainer on 2026-10-24:
refuse the rendition rather than escape it. Percent-encoding does not help,
since WHATWG URL parsing treats `%2e%2e` as a dot segment too, and a reversible
escape would burden every URL-building path for names no real publisher uses.

- Refuse where renditions are admitted from the catalog, so the master
  playlist, the DASH manifest, and the router agree.
- Test that `.` and `..` renditions are absent from both manifests and the
  others are served.

Public API: none. Wire: none.
