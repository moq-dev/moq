# [M] CMAF tracks with in-band parameter sets round-trip

## Goal

An fMP4 `avc3` or `hev1` track, whose parameter sets travel in the samples
rather than the sample entry, imports through `fmp4::Import` and decodes
through `moq-video`, so `moq export fmp4` output can be read back. Today
`fmp4::Import` refuses `avc3` (`Cmaf(UnsupportedCodec(Avc3))`), and
`moq-video`'s `Decoder::new` reads `H264 { inline: true }` and
`H265 { in_band: true }` as Annex-B start codes (`Conversion::Passthrough`),
while CMAF samples are length-prefixed. An `hev1` CMAF track likely decodes
wrongly today through the same path.

## Plan

Decided (maintainer, 2026-10-07): this lands before
[fMP4 init from the catalog](/quest/m1/fmp4-catalog-init.md) (#5015), which
starts writing `avc3` and broke `moq-video`'s
`decode::consumer::tests::reads_cmaf_container_declared_by_catalog`.

- `fmp4::Import` accepts `avc3` like `hev1`, with an empty or partial
  configuration record and parameter sets read from the samples.
- The decoder picks its NAL conversion from the catalog container, not only
  from `inline`/`in_band`: a CMAF track is length-prefixed (length size from
  the configuration record), a Legacy or LOC Annex-B track is start-coded.
- Check `@moq/hang`/`@moq/watch` for the same assumption and fix it in the same
  PR if it is there.

Test: export Annex-B H.264 and H.265 to fMP4 as `avc3`/`hev1`, import it, and
decode it in `moq-video`, plus an existing `hev1` CMAF fixture through the
decoder.

## Related

- [fMP4 init from the catalog](/quest/m1/fmp4-catalog-init.md) - requires this
