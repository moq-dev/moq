# [S] FLV export takes a catalog stream

## Goal

`flv::Export` is built from a catalog stream like `fmp4::Export`, so
callers narrow renditions with `catalog::Stream::select` and the FLV-only
`with_select` builder is gone.

## Plan

This is a published API break. Consider whether the TS
and Matroska exports should take the same shape in the same pass. RTMP play
passes its client-capability selection through the stream instead.
