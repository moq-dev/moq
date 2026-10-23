# [XS] An MSF catalog refuses duplicate init data ids

## Goal

An MSF catalog that repeats an `initDataList` id is refused as a decode error by every in-repo reader (`rs/moq-msf` and `js/msf`), instead of resolving to whichever entry comes last.

Today `rs/moq-msf/src/lib.rs` collects the list into a `HashMap`, and `js/msf/src/catalog.ts` into a `Map`, so a repeated id silently replaces the earlier entry. The strict duplicate-key pass used for `moq-json` catalogs does not cover this path. Two readers that keep different entries hand different decoder initialization bytes to different parties for the same broadcast.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Reject a duplicate id while building `by_id` with `serde::de::Error::custom`, the path the catalog's other semantic checks already use.
- Reject it the same way in `js/msf`'s catalog decoder.
- Cover the malformed catalog in both suites with the example from the audit: two entries named `v` with different `data`, and a track that names `v`.

Public API: none. Wire: none.

## Related

- [strict JSON](/quest/m1/compress/README.md) - the duplicate-key rule this extends to the MSF path
