# [XS] HLS import arithmetic cannot overflow on hostile playlist numbers

## Goal

No arithmetic on a playlist-supplied number in `rs/moq-hls/src/import.rs` can
panic or wrap, whatever a master or media playlist declares.

Today two paths do:

- `select_variants` computes `width * height` from `RESOLUTION`, which panics
  on a checked build and wraps on a release build, misordering the variants.
- `anchor` (and `ingest`) compute `media_sequence + segments.len()` before any
  validation, so a media playlist at `EXT-X-MEDIA-SEQUENCE:18446744073709551615`
  with one segment panics on a checked build. A release build wraps and
  `SequenceOverflow` catches it later, by luck.

## Plan

From the 2026-10-22 security audit; the sequence case was added from the
2026-10-24 moq-hls audit, since it is the same problem.

- Compute the area as an exact widened product (for example `u128` from the
  two `u64` dimensions), so every resolution ranks by its true area.
  Saturating would collapse large areas into ties that bandwidth and map order
  then break arbitrarily.
- Validate a media playlist's sequence span (checked addition, refused as
  `SequenceOverflow`) before anchoring, fetching, or any other arithmetic on
  it, and fix every instance of the unchecked sum.
- Tests: an enormous `RESOLUTION` covers both the panic and the ordering; a
  sequence at `u64::MAX` with a segment is refused, not panicked on.

Public API: none. Wire: none.
