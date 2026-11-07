# [XS] moq-mux reads only the base layer's H.265 SPS

## Goal

A layered H.265 stream (MV-HEVC, such as spatial video, or SHVC) imports as its
base layer: moq-mux configures the catalog only from an SPS whose
`nuh_layer_id` is 0, in Annex-B (`find_sps` and the splitter's `SpsNut` arm)
and in hvcC (`apply_hvcc_sps`). A stream or record with no base-layer SPS is
refused with an error naming that, not misread. A fixture test with an
enhancement-layer SPS covers it.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from a follow-up of
[moq-dev/moq#8140](https://github.com/moq-dev/moq/pull/8140), which replaced
scuffle-h265's SPS reader with moq-mux's own. An SPS with `nuh_layer_id > 0`
uses a different layout (`sps_ext_or_max_sub_layers_minus1`, H.265
F.7.3.2.2.1). Both scuffle and the new reader read it with the base-layer
layout, so it can yield garbage metadata or a spurious parse error. Today
`is_sps` (`rs/moq-mux/src/codec/h265/import.rs`) and the hvcC lookup match
on `nal_unit_type` alone.

Decided 2026-11-07 (maintainer): ✅ base layer only. Not chosen: reading the
layered layout too, which has no consumer yet; or m3.

Check the second header byte (`nuh_layer_id` spans the low bit of byte 0 and
the high five bits of byte 1) wherever an SPS is picked. Keep enhancement-layer
NALs flowing through to the track unchanged; only the catalog reads the base
layer.
