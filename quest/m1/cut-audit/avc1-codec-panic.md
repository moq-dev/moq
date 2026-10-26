# [XS] A malformed avc1 codec string is refused, not a panic

## Goal

A catalog whose `avc1`/`avc3` codec string has a non-ASCII or non-hex
profile part fails to parse with `Error::InvalidCodec` in every Rust
catalog reader, instead of panicking on a string slice. No catalog reader
across the C ABI can be made to abort by a publisher.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced, inherited.

`rs/hang/src/catalog/video/h264.rs:50` checks `part.len() != 6` in bytes and
then slices `&part[0..2]`, so `"avc1.aé123"` (6 bytes) panics inside `é`.
It runs on every catalog deserialize through `VideoCodec: FromStr`,
including the moq-mux hang and MSF consumers
(`rs/moq-mux/src/catalog/msf/consumer.rs:283`), so moq-hls, moq-cli,
moq-gst, moq-ffi, and moq-c are exposed. The relay does not parse catalogs.

Check each byte is an ASCII hex digit before slicing (this also refuses the
leading `+` that `from_str_radix` accepts). Audit the other codec string
parsers for any slice before an ASCII check; the audit found H.265, VP9,
AV1, and AAC already safe. Add the input to the catalog parse tests.

Public API: none. Wire: none.
