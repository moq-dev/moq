# [L] One handle publishes a rendition

## Goal

Publishing one media track through `moq-mux` is one object: it writes
frames, reports its estimate, reconfigures its catalog entry, and retires
the entry when dropped. Today it is five (`Reserved`, `Rendition`,
`moq_net::track::Producer`, `container::Producer`, and the `Estimate`
shuttled between the last two), every codec importer calls
`rendition.estimate(track.estimate())` by hand on each cut, and moq.pro's
recorder keeps a `_registration` field alive purely so `Rendition::drop`
does not retire the catalog entry.

## Plan

Decided 2026-09-18: `container::Producer<C>` owns its `Rendition`.
`Reserved::video(&self, track, container, config) -> Result<container::Producer<C>>`
(and `Producer::video(...)` for the unreserved path, audio and text alike)
publishes the estimate on `cut`/`finish`, exposes `modify()` for
reconfiguration, and retires the entry on drop. `Rendition`, `VideoTrack`,
`Reserved::producer()`, `Producer::media_producer`, and `Producer::enroll`
go private; the fMP4 passthrough that writes groups by hand reaches
`enroll` internally.

Either way `Rendition::update(&mut self, FnOnce(&mut C))` becomes a
`modify() -> Result<Guard>` that errors with `NotPublished` before `set`
instead of a callback that silently no-ops.

Public API: breaking on moq-mux and the crates that publish through it
(moq-audio, moq-video, moq-ffi, libmoq, moq-cli, moq-gst, the gateways),
so on dev. Wire: none. Consumers here plus moq.pro's recorder and overlay.

## Related

- [Rendition ownership #2869](/quest/m1/merge-dev.md) - the earlier decision that made `Rendition` the sole catalog writer, which this keeps
