# [M] A data track's declared break is not a loss

## Goal

A consumer tells a data track's declared discontinuity from media lost
upstream. `moq export ts` stops counting a declared break on a verbatim PES
track (DVB AC-3, teletext, SCTE-35, ID3) in `stats::Export::gaps`, stops
warning "media skipped upstream" for it, and no longer trims what such a
track holds when the break lands before its first frame goes out.

## Plan

Found in [#5713](https://github.com/moq-dev/moq/pull/5713). Audio and video
mark a break with one empty frame, which the consumer reads as `Read::End`.
An empty payload is data on a data track, so `Producer::discontinuity`
(`rs/moq-mux/src/container/producer.rs`) skips a sequence with no object
instead. The consumer reads that skip as `Read::Gap`, the same as a lost
group, so the export's jitter buffer, gap count and warning treat it as loss.
#5713 documents the overlap rather than fixing it.

Settle the carriage first: the marker must be visible on the wire, and an
empty group (zero objects) already means nothing. Any wire or format change
updates `drafts/` and `js/hang` in the same PR. The regression is
`export_drops_an_ac3_frame_cut_by_a_marker` (`rs/moq-mux/src/container/ts/export_test.rs`),
which should keep frame 0 again once the break is told apart.

## Related

- [Loss and recovery tests](/quest/m1/ts-reliability/recovery.md) - the loss reporting this keeps honest
