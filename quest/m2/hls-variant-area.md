# [XS] HLS variant ranking cannot overflow on a hostile resolution

## Goal

Ranking a master playlist's variants by `RESOLUTION` cannot panic or wrap, whatever dimensions the playlist declares.

Today `select_variants` in `rs/moq-hls/src/import.rs` computes `width * height` on playlist-supplied values, which panics on a checked build and wraps on a release build, misordering the variants.

## Plan

From the 2026-10-22 security audit. Compute the area without overflow (saturating or widened arithmetic), so an absurd resolution ranks as the largest rather than an arbitrary one. A test with an enormous `RESOLUTION` covers both the panic and the ordering.

Public API: none. Wire: none.

## Related

- [MKV import refuses out-of-range values](/quest/m2/mkv-dimension-overflow.md) - the same class of catalogue-field overflow in another importer
- [HLS admission](/quest/m1/release-audit/hls-admission.md) - the other HLS import hardening from the audit
