# [S] FLV export takes a catalog stream

## Goal

`flv::Export` is built from a catalog stream like `fmp4::Export`, so
callers narrow renditions with `catalog::Stream::select` and the FLV-only
`with_select` builder is gone.

## Plan

This is a published API break. The Matroska and fMP4 exports already take a
catalog stream (`mkv::Export::new(source, catalog)`), so only TS is open for
the same pass: consider whether it should take the same shape too. RTMP play
passes its client-capability selection through the stream instead.
