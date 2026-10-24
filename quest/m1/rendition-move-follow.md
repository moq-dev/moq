# [M] Catalog readers follow a moved rendition

## Goal

When a catalog moves a rendition to a new track, every reader that picked
it keeps playing from the new track, not only the exporters.

## Plan

[moq-dev/moq#6276](https://github.com/moq-dev/moq/pull/6276) made the
exporters rebind. Other readers resolve a rendition's track once:
`rs/moq-rtc/src/egress.rs` subscribes per pick, and moq-ffi's audio and video
consumers, moq-gst's source, and `moq play` are candidates to check. Audit
each, make the ones that resolve once follow catalog updates, and test a move
mid-playback for each.

Broadcast restarts are out of scope: readers already follow those through
`moq_mux::follow::Instances`, or stay on the old instance until its route dies.

Public API: none expected. Wire: none.

## Related

- [One Annex-B exporter](/quest/m1/annexb-export.md) - its shared lifecycle also follows a rebound rendition
