# [XS] A failed catalog finish is final

## Goal

`moq_mux::catalog::Producer::finish` is never half-finished: when finishing
either catalog track fails, the producer reports that failure for good and
both catalog tracks end. It is never left closed with a track still open.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8210](https://github.com/moq-dev/moq/pull/8210), which fixed
this shape in the importers and the container, audio, and JSON producers
but left the catalog producer outside its list.

`finish` (`rs/moq-mux/src/catalog/producer.rs`) stores
`closed = Some(Error::Closed)` before `self.outputs.hang.finish()?` and
`self.outputs.msf_track.finish()?`. If the hang track's finish fails, the
MSF track is never finished, yet every later call reports a clean `Closed`.
Record the terminal outcome after the fallible work, as #8210 did: keep the
first error, abort or end the other track, and store the failure rather
than `Closed`. Test a failing track finish.

A handle created after the failed finish reports that same first error, not
`NotPublished`: a `catalog::Producer::video(..., None)` (or `audio`/`text`)
container producer, and its `modify()`, `set`, and the other calls that
reach the catalog. Test one created after the failure.

Public API: none. Wire: none.
