# [S] HLS import refuses a byte range the origin ignored

## Goal

An HLS import that requests an `EXT-X-BYTERANGE` segment accepts only a `206`
whose `Content-Range` matches the request, and never holds more than the
requested length of a ranged body. Today an origin that ignores `Range` and
answers `200` makes the import buffer the whole file and slice it
(`slice_full_response` in `rs/moq-hls/src/import.rs`), once per segment.

## Plan

From the 2026-10-24 moq-hls audit. Decided with the maintainer on 2026-10-24:

- Refuse a non-`206` answer to a ranged request with a loud error, rather than
  reading a `200` body up to the range end, which tolerates a broken origin at
  the cost of reading every earlier byte for each segment.
- Stream a ranged body and fail as soon as it exceeds the requested length,
  instead of `response.bytes()` followed by a length check.
- Non-ranged segments and playlists keep today's behavior.
- Tests: a `200` to a ranged request is refused; a `206` that overruns its
  length fails before buffering the excess; a matching `206` imports.

Public API: an error variant, if the existing ones do not name the refusal.
Wire: none.
