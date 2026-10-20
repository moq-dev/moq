# [S] History from the retained floor

## Goal

An HLS export in history mode lists an archive consumer broadcast whose
oldest objects were deleted, starting at its oldest stored segment. Today
`moq_hls::export::Config::history` refuses a pruned start with
`Error::HistoryPruned` before listing anything, because the exporter cannot
tell a pruned start from missing history. A gap after the floor still fails,
never listing as a complete recording.

## Plan

Decided 2026-10-11 (#5236): refuse pruned recordings now, and quest the floor.

Decided 2026-10-17: kept as its own `moq-hls` quest rather than folded into
the [Archive consumer](/quest/m1/archive/consumer.md). The consumer only
answers a group below its oldest stored key as dropped; this quest makes
history mode start there. The old Reader's timeline-map pruning and any
retention field in the catalog are gone with the reader and the root
`archive` section. The archive producer's DVR mode is what prunes the start.

History mode reads the archived `hls` track's groups from group 0. Start
at the first `hls` group the broadcast delivers, using its checkpoint's
retained window, and skip records whose first media group is below the floor.
Once [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) lands, an
`Absolute(0)..` range answers from the oldest group that exists and names the
rest with SUBSCRIBE_DROP; before that, the consumer's drop serves the same
purpose.

Test an archive whose oldest objects are deleted: history lists from the first
stored segment, and a hole after it still fails.

## Required

- [Archive consumer](/quest/m1/archive/consumer.md) - serves the pruned archive and drops groups below its floor
- [Catalog HLS field](/quest/m1/archive/catalog-hls.md) - the `hls` records history mode reads

## Related

- [Failed HLS exports error](/quest/m1/hls-failure-signal.md) - the other history-mode gap left by #5236
- [DVR rewind](/quest/m2/archive-dvr.md) - seeks through the same bounded archive
