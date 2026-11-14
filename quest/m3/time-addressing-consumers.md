# [M] Media consumers address groups by start

## Goal

hang stops writing the duration and discontinuity markers that group ends
and gaps now carry, and the hang draft says so. Every first-party consumer
that numbered groups works by start timestamp, in Rust and JS, and the
HLS/DASH export places each group on the declared `media.join.interval` grid
by its start.

## Plan

Moved from m2 to m3 2026-11-14 (the m2 triage before the cut, under
/quest-afk, for the maintainer's review): ✅ m3, since what remains breaks
nothing (rejected: m2; splitting a breaking part out, since none is left).
[The model](/quest/m2/time-addressing/model.md)
([moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638)) converts the
Rust consumers whose API named group numbers: `moq fetch --group` and the
relay's `/fetch?group=` keep their names and take a start, moq-mux `seek` and reads carry starts,
and moq-hls, moq-archive, and moq-transcode key by start. `@moq/hang`'s
`Read.group` keeps its name and carries whatever `@moq/net` keys by. Dropping
the markers is no break either: readers already tolerate their absence
(2026-10-25, below). What remains: the markers, the JS consumers'
continuity checks once [@moq/net by start](/quest/m2/time-addressing/js.md)
lands, the hang draft, and a pass over the list below for anything the model
left numbering groups. Resized from L to M with it.

Decided 2026-10-25 (maintainer), from the spike:

- GROUP_END replaces hang's empty-frame duration marker
  (`rs/moq-mux/src/container/producer.rs`, `cut` and `close`), so moq-mux
  stops writing empty frames.
- Discontinuity marker groups go away (`producer.rs`, `discontinuity`): a gap is a
  previous end below the next start, and a missing group is a
  previous-start link that doesn't match.
- Legacy egress synthesizes neither: old players lose end durations and
  pause markers, which they already tolerate being absent.

Decided 2026-11-02 (maintainer): removing the HLS records, the per-rendition
`hls` field, and A/V segment alignment moved to m0 as the HLS grid,
landed in [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652), which also added
`media.join.interval`, moved `clock` to `media.clock`, and settled the
segmentation rules decided here on 2026-10-27/28. It places groups by their
first frame's pts; this quest swaps that for the group start, without
changing the segment numbers.

Convert each consumer that numbers groups: hang and moq-mux timelines,
HLS/DASH export, `seek` for GOP-aligned failover in moq-mux, HLS import's
discontinuity packing, transcode's 1:1 numbering, archive keys and resume,
relay and CLI `fetch --group`, and player continuity checks in the Rust and
JS container consumers and `js/watch`. Update the hang draft and the docs
each one makes stale. Rename no CLI flag: that breaks callers, which m3
cannot.

Public API: none left. The model keeps `moq fetch --group` and the relay's
`/fetch?group=`, whose value now names the group starting there, and keys
archives by start.
Wire: hang drops empty duration frames and discontinuity marker groups, which
readers already tolerate.

## Required

- [Rust groups by start](/quest/m2/time-addressing/model.md) - the Rust API these consume
- [@moq/net by start](/quest/m2/time-addressing/js.md) - the JS API these consume
- [Lite-07 by start](/quest/m2/time-addressing/lite07.md) - GROUP_END carries the end the duration marker did
