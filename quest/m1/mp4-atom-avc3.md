# [XS] mp4-atom ships avc3

## Goal

A published `mp4-atom` release decodes and encodes an `avc3` sample entry:
`Avc3` and `Codec::Avc3`, the `avc1` body with parameter sets allowed in-band
and an avcC that may list none. Today `Codec::Unknown` refuses to encode, so
moq-mux can't write an avc3 entry without patching bytes.

This quest tracks a condition outside the repository. Advance it by reviewing
and merging [kixelated/mp4-atom#72](https://github.com/kixelated/mp4-atom/pull/72),
then the release-plz PR it triggers. When a release on crates.io carries
`Avc3`, delete this quest and every `Required` entry that links it.

## Plan

As of 2026-10-06, #72 is rebased on mp4-atom `main` and passes `just check`
and the tests locally. `Avc3` is a plain struct mirroring `Avc1`, as `Hev1`
mirrors `Hvc1`; deduplicating the AVC and HEVC entries is left to a follow-up
like kixelated/mp4-atom#109. The avcC stays required (ISO/IEC 14496-15
5.4.2.1.1), which answers the maintainer's standing change request on #72.
The API change is additive (`Codec` is `#[non_exhaustive]`), so it fits a
0.16 patch release.

## Related

- [Bump mp4-atom for avc3](/quest/m1/mp4-atom-avc3-bump.md) - the bump that follows the release
