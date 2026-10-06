# [S] mp4-atom avc3 sample entry

## Goal

A released `mp4-atom` decodes and encodes an `avc3` sample entry (the `avc1`
body, with parameter sets allowed in-band and an avcC that may list none), and
this repository bumps to that release.

## Plan

The work lands in kixelated/mp4-atom: an `Avc3` atom and `Codec::Avc3`
variant mirroring `Avc1`. Today `Codec::Unknown` refuses to encode, so moq-mux
can't write an avc3 entry without patching bytes. The bump here is a separate,
small step once the release ships.

## Related

- [fMP4 init from the catalog](/quest/m1/fmp4-catalog-init.md) - the export that writes avc3 entries
