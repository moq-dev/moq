# [M] HLS import holds a bounded amount of each response

## Goal

moq-hls import never holds a whole HTTP body it doesn't need whole. Media
segments stream into the fMP4 importer as they arrive, with backpressure, so a
large or endless segment body costs no extra memory. Playlists and init
segments, which must be held whole, are refused once they pass a private
bound. A misbehaving origin or CDN can then no longer exhaust the process's
memory within the 30 s request timeout.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`):

- Milestone: ✅ m1 (rejected: m3), audit score 7/10.
- Approach: ✅ stream media and cap metadata (rejected: a byte cap on every
  body). Reason: streaming bounds media by design, and only what must be held
  whole gets a constant, which matches
  [one policy for oversized container metadata](/quest/m3/import-metadata-policy.md).

Facts at `9f51fa0966`:

- `rs/moq-hls/src/import.rs:285` calls `response.bytes()` for every
  unranged fetch: playlists (`:244`, `:969`), init segments (`:613`) and
  media (`:650`). `file://` sources read the whole file (`:262`). The ranged
  path (`:296-311`) already reads chunk by chunk, so start from it.
- The origin is operator-configured and redirects are pinned to it
  (`:177`, `:203-212`), but the code treats playlists as untrusted (`:189`).
- `fmp4::Import::decode` (`rs/moq-mux/src/container/fmp4/import.rs:352-480`)
  is incremental: samples stream into frames, and only `moov`/`moof` bodies
  are held, capped at `MAX_METADATA_BYTES`.
- Today a failed fetch leaves the importer untouched, so the step retry
  re-fetches cleanly. Once a segment is half decoded, a failed transfer must
  evict that init generation, as decode errors already do (`:695`), and
  accept the gap. Update the retry tests to match.
- Size the playlist and init bounds from what real playlists and init
  segments need, keep them private, and refuse with an error past them.

Public API: none, or a new error variant in moq-hls. Wire: none.

## Related

- [One policy for oversized container metadata](/quest/m3/import-metadata-policy.md) - the importer-side bounds this aligns with
