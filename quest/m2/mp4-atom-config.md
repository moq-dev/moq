# [S] avcC and hvcC through mp4-atom

## Goal

moq-mux parses and builds avcC and hvcC records with `mp4_atom::Avcc` and
`mp4_atom::Hvcc`, already its dependency, instead of about 330 hand-written
lines, or a comment at the hand-written code names the input mp4-atom
mishandles.

## Plan

Found by the 2026-10-19 dependency audit. `rs/moq-mux/src/codec/h264/mod.rs`
has `Avcc::parse`, `build_avcc`, `catalog_avcc`, `avcc_params`, and
`read_param_sets`; `codec/h265/mod.rs` has `build_hvcc` and `hvcc_params`.
The tests already check them against mp4-atom. The fMP4 importer decodes an
`mp4_atom::Avcc`, re-encodes it, and re-parses it with the hand-written parser
(`container/fmp4/import.rs`, `init_h264`), while its H.265 path reads the
`mp4_atom::Hvcc` fields directly.

Check first for a leniency reason, such as how mp4-atom handles the
high-profile extension or a short record from a real encoder; keep the
hand-written path only for a reproduced case and test it. `moq_mux` exports
`codec::h264::Avcc`, so replacing or removing it is a breaking change;
prefer keeping the type and changing only how it is
filled when that is simpler. No wire change.

## Related

- [H.264 parsing with maintained crates](/quest/m2/h264-reader.md) - the SPS side of the same codec config
