# [S] HLS import trusts only its playlist's origin

## Goal

The HLS importer follows nothing a playlist names outside the origin it was configured with:

- `resolve_uri` accepts a relative reference, or an absolute URL whose scheme and authority equal the playlist's.
- `file:` is read only when the playlist itself is a `file:` URL.
- A variant's `RESOLUTION` multiplication cannot overflow.

Today `rs/moq-hls/src/import.rs` returns any absolute URL verbatim, and `Fetcher::fetch` opens `file:` paths and issues arbitrary GETs. A hostile or compromised playlist can therefore read local files and reach the operator's internal network, and `width * height` can panic on a debug build.

This code is on `release`, so the fix lands on `main` and is ported onto `release` as a separate PR that keeps the published API.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Check scheme and authority in `resolve_uri`, not at each fetch site, so `EXT-X-MAP`, every segment, and future references share one rule.
- Keep `file:` for a `file:` playlist, which is how local test fixtures work.
- Compute the variant area with `saturating_mul`.

Tests: a playlist naming `file:///etc/passwd`, an instance-metadata address, and a cross-origin map and segment is refused; a relative segment still resolves; an enormous `RESOLUTION` does not panic.

Public API: none. Wire: none.

## Related

- [Focused release backports](/quest/m0/release-backports/README.md) - the release port joins this shortlist once the main fix lands
- [MKV dimension overflow](/quest/m2/mkv-dimension-overflow.md) - the same class of catalogue-field overflow in another importer
