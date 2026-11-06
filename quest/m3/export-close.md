# [M] Container export ends cleanly when its broadcast closes

## Goal

A container export (FLV, MKV, TS, fMP4, or MP4) whose broadcast instance
closes in an orderly way returns `None`, as each one's docs promise ("`None`
when the broadcast ends"), after the media it already read, whichever of the
track's shutdown error and the instance's close arrives first. A failure that
is no orderly shutdown, such as malformed media or a failed transport, still
fails the export.

## Plan

Decided 2026-11-06 (maintainer, on moq-dev/studio's /quest-plan): m3, since
the studio is a named consumer that is not urgent.

A broadcast's orderly end reaches an export by three paths, and each must
end it cleanly whichever signal comes first:

- **Renditions on the source's own broadcast**, the studio's case.
  `ExportSource::poll_track` (`rs/moq-mux/src/container/source.rs`) sets
  `instance` only for a sibling (`sibling: *path != self.path` in
  `rs/moq-mux/src/source.rs`), so these never consult `poll_closed` and any
  shutdown-class error fails the export.
- **Sibling renditions.** `poll_track` ends one on a shutdown-class error
  (`Cancel`, `Closed`, `GoingAway`, a session cancel) only if
  `instance.poll_closed` is already ready.
- **The catalog track**, which every exporter polls before its media (for
  fMP4, `catalog.poll_next(waiter)?` in `container/fmp4/export.rs`). The
  catalog consumer returns `Cancel` when its track is lost
  (`a_lost_track_is_a_transport_error` in `catalog/hang/consumer.rs`), so an
  orderly leave can fail the export there before any media is read.

Fix them in the shared source and the catalog consumer, so every exporter
gets it; `mp4::Export` ends when the fMP4 export under it does. Buffered
media still drains before `None`. Prove it through whole exports, where the
catalog's and the media's shutdown arrive in both orders against the
instance's close.

Open: how the error-first order resolves. Holding a shutdown-class error
until the instance closes cannot be the whole answer: nothing bounds that
wait if the instance stays live, and no timeout may decide it. Prefer fixing
the order at its source, so an instance's end is visible before its tracks
(catalog included) report shutdown, locally and over the wire, if moq-net can
promise that; otherwise find what else tells the two apart. Reproduce both
orders first.

A failure that is no orderly shutdown (malformed media, a protocol violation,
producers that went away unfinished) still fails the export even once the
instance has gone, as `a_sibling_failure_outlives_its_instance` pins, and
`a_remote_sibling_ends_with_its_instance` covers the remote case.

Consumer: the studio's per-guest recorder
([moq-dev/studio#65](https://github.com/moq-dev/studio/pull/65)) treats an
export error as the guest leaving only if the broadcast already reads as
closed (`is_closed` in `rs/moq-studio/src/record.rs`), so a departure whose
track error arrives first marks the recording failed. The studio drops that
check when it next bumps `moq`.
