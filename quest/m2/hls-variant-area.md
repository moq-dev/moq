# [XS] HLS variant ranking cannot overflow on a hostile resolution

## Goal

Ranking a master playlist's variants by `RESOLUTION` cannot panic or wrap, whatever dimensions the playlist declares.

Today `select_variants` in `rs/moq-hls/src/import.rs` computes `width * height` on playlist-supplied values, which panics on a checked build and wraps on a release build, misordering the variants.

## Plan

From the 2026-10-22 security audit. Compute the area as an exact widened product (for example `u128` from the two `u64` dimensions), so every resolution ranks by its true area. Saturating would collapse large areas into ties that bandwidth and map order then break arbitrarily. A test with an enormous `RESOLUTION` covers both the panic and the ordering.

Public API: none. Wire: none.
