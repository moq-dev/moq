# [M] A data track's declared break is not a loss

## Goal

A consumer tells a data track's declared discontinuity from media lost
upstream. `moq export ts` stops counting a declared break on a verbatim PES
track (DVB AC-3, teletext, SCTE-35, ID3) in `stats::Export::gaps`, and stops
warning "media skipped upstream" for it.

## Plan

Found in [#5713](https://github.com/moq-dev/moq/pull/5713). Audio and video
mark a break with one empty frame, which the consumer reads as `Read::End`.
An empty payload is data on a data track, so `Producer::discontinuity`
(`rs/moq-mux/src/container/producer.rs`) skips a sequence with no object
instead. The consumer reads that skip as `Read::Gap`, the same as a lost
group, so the export's gap count and warning treat it as loss. #5713
documents the overlap. Its start trim already follows only gaps that cut a
group short, and the break comes after its group ended, so nothing is trimmed.

Settle the carriage first: the marker must be visible on the wire, and an
empty group (zero objects) already means nothing. Any wire or format change
updates `drafts/` and `js/hang` in the same PR. Extend
`export_drops_an_ac3_frame_cut_by_a_marker` (`rs/moq-mux/src/container/ts/export_test.rs`)
to assert `gaps` stays 0 across the break.

## Related

- [Loss and recovery tests](/quest/m1/ts-reliability/recovery.md) - the loss reporting this keeps honest
