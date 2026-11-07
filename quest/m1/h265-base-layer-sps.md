# [S] moq-mux reads only the base layer's H.265 SPS

## Goal

A layered H.265 stream (MV-HEVC, such as spatial video, or SHVC) imports as its
base layer. Every place moq-mux picks an SPS to configure the catalog takes
only one whose `nuh_layer_id` is 0, through one shared predicate. An
enhancement-layer SPS still reaches the track unchanged, including on later
bare keyframes, and never evicts the base layer's SPS from a cache. A stream
whose SPSs are all enhancement-layer is refused with an error naming that, not
misread. An hev1 record that lists no SPS still defers to the in-band one, as
today. Fixture tests cover the catalog, both import paths, and the
enhancement SPS reaching the track.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from a follow-up of
[moq-dev/moq#8140](https://github.com/moq-dev/moq/pull/8140), which replaced
scuffle-h265's SPS reader with moq-mux's own. An SPS with `nuh_layer_id > 0`
uses a different layout (`sps_ext_or_max_sub_layers_minus1`, H.265
F.7.3.2.2.1). Both scuffle and the new reader read it with the base-layer
layout, so it can yield garbage metadata or a spurious parse error. Today
every SPS lookup matches on `nal_unit_type` alone.

Decided 2026-11-07 (maintainer): ✅ base layer only. Not chosen: reading the
layered layout too, which has no consumer yet; or m3.

Selection sites found in planning review (all in `rs/moq-mux/src/codec/h265/`
unless noted):

- hvcC: `apply_hvcc_sps` and `config_from_hvcc` (`Hvcc::parse(..).sps.first()`),
  which the hvc1 init path, the FLV importer (`container/flv/import.rs`), and
  `codec_dimensions` (`container/source.rs`) reach;
- Annex-B: `find_sps`, and `initialize_hev1`, which seeds from every SPS in its
  buffer, so the last one, possibly an enhancement layer, wins.

The splitter's `SpsNut` arm (`split.rs`) is retention and re-injection, not a
catalog site. Its `ParameterSets` map is keyed only by
`sps_seq_parameter_set_id`, and `insert_parameter_set` reads that id with the
base layout, so an enhancement SPS can fail to parse or overwrite the base SPS
with the same id. Key it by layer as well as id, so both are retained and
re-injected.

`nuh_layer_id` is six bits across both header bytes:
`((nal[0] & 0x01) << 5) | (nal[1] >> 3)`. A NAL is base-layer when
`nal[0] & 0x01 == 0 && nal[1] & 0xF8 == 0`, after the length check.
