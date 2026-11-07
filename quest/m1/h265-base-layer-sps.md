# [XS] moq-mux reads only the base layer's H.265 SPS

## Goal

A layered H.265 stream (MV-HEVC, such as spatial video, or SHVC) imports as its
base layer: moq-mux configures the catalog only from an SPS whose
`nuh_layer_id` is 0, in Annex-B (`find_sps` and the splitter's `SpsNut` arm)
and in hvcC (`apply_hvcc_sps`). An enhancement-layer SPS still reaches the
track unchanged, through the splitter's parameter-set cache and any
initialization cache. A stream whose SPSs are all enhancement-layer is refused
with an error naming that, not misread; an hev1 record that lists no SPS still
defers to the in-band one, as today. Fixture tests cover both the catalog and
the enhancement SPS reaching the track.

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
the high five bits of byte 1) wherever an SPS is picked. The splitter's
`insert_parameter_set` reads the SPS id, which sits elsewhere in the layered
layout (with `sps_ext_or_max_sub_layers_minus1` at 7 the base-layout read
fails), so key an enhancement-layer SPS by its layer and id, or pass it through
without parsing, rather than refusing it. Only the catalog reads the base
layer.
