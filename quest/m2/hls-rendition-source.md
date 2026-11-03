# [S] moq-hls exposes each rendition's bound source

## Goal

A moq-hls export reports, for every rendition it serves (including
renditions that reference another broadcast's track), the source broadcast
path and route epoch that rendition is currently bound to. A caller can then
key a cache or an authorization decision on the actual source rather than on
the requesting viewer's grants.

## Plan

Decided 2026-11-03 (maintainer, `/quest-plan`): m2. A downstream HLS cache
keys entries on a digest of each viewer's subscribe scope, because it can't
tell which source a referenced rendition reads. Viewers holding different
grants for the same broadcast then never share a cached entry. Correctness is
already handled; this recovers the sharing.

- Expose the binding from the export (`rs/moq-hls/src/export/`, where
  `Renditions::sync` resolves sources against the catalog), for example on
  each rendition's playlist or segment response metadata, so an HTTP server in
  front of it can surface it.
- Report a change of binding, such as a publisher replacement's new epoch, as
  a new value rather than reusing the old one.
- Test a local rendition, a referenced rendition, and a rebinding across a
  replacement.
