# [S] moq-hls exposes each rendition's bound source

## Goal

A moq-hls export reports, for every playlist and segment it serves (including
renditions that reference another broadcast's track), the source broadcast
path and instance that supplies it. A caller can then key a cache on the
actual source rather than on the requesting viewer's grants.

## Plan

Decided 2026-11-03 (maintainer, `/quest-plan`): m2. A downstream HLS cache
keys entries on a digest of each viewer's subscribe scope, because it can't
tell which source a referenced rendition reads. Viewers holding different
grants for the same broadcast then never share a cached entry. Correctness is
already handled; this recovers the sharing.

- Identify a source by its bound broadcast's `path` and `instance`
  (`moq_net::broadcast::Info`). A content replacement mints a new instance.
  The route epoch stays hidden: it is `pub(crate)` in moq-net, and a higher
  epoch continues the same instance's sequences, so keying on it would split
  a shareable entry for no correctness gain.
- Expose it from the export (`rs/moq-hls/src/export/`, where
  `Renditions::sync` resolves sources against the catalog), for example as
  response metadata, so an HTTP server in front of it can surface it.
- Report the binding that supplies each resource, not only the rendition's
  current one. After a replacement, a rendition keeps listing its
  predecessors' rows (`Rendition::after` in `export/rendition.rs`), and a
  predecessor's binding supplies those segments' bytes. A segment reports the
  source it is muxed from, and a playlist reports every source its listed
  segments read.
- The source identity replaces the viewer's grants only in the cache key: a
  viewer is still authorized against the source before a cached entry is
  served to it.
- Report a change of binding, such as a publisher replacement's new instance,
  as a new value rather than reusing the old one.
- Test a local rendition, a referenced rendition, and a replacement while the
  predecessor's rows still drain, so one playlist lists both sources.
