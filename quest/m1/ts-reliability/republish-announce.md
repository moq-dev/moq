# [M] A republish announces as soon as its tracks are known

## Goal

After a TS import republishes on a clock step, the new instance's catalog,
and so its announcement, goes out as soon as every elementary stream's
config is known, with no extra wait. A stitched export then joins the new
instance's first group while it is still the newest, so a short-GOP feed
loses only the truncated picture at the cut, not a whole GOP.

## Plan

Decided 2026-11-03 (user, /quest-import of
[moq-dev/moq#7744](https://github.com/moq-dev/moq/issues/7744)).

- ✅ Remove the import-side wait beyond the last stream's config. Not chosen:
  announce at once with the predecessor's track list (the catalog is wrong
  when the switch changed a codec config, and a relay could query tracks
  the instance cannot serve yet). Not chosen: start a stitched replacement at
  its first group (output would stay behind `--delay` by the announce lag for
  the rest of the stream, against #7049). Not chosen: won't fix.
- Milestone: ✅ m1, in this epic. Not chosen: m2.
- Accepted: a feed whose stream configs arrive after its second keyframe
  still loses its first group. That cost comes from the source.

[Draft implementation](https://github.com/moq-dev/moq/pull/7780) reproduced
the delay with a paced seven-stream, 1.2 s GOP source. After a rewind,
video was the final reservation because its first keyframe lost the
PAT/PMT already routed before the step. With that map available, MP2's
complete config still waited for its enclosing PES to end. A configured
track could also wait beyond the PES header that finalized its clock.

Incremental decoded-media parsing and releasing the initial clock
reservation at the first PTS address those waits. Paused-clock tests cover
early catalog publication, exact audio frames and timestamps, and the
stitched export's first replacement picture and last old picture. The
announcement comment is corrected, and a restart benchmark sweeps program
count and bitrate.

Remaining: replace the draft's synthetic program-map prefix with a handoff
that preserves the first media without treating the predecessor's map as
the new source's authoritative layout. Local review found that a source
whose PCR precedes its new PAT can inherit the old identity and then fail
when the new identity arrives. A PMT-configured old track can also announce
before the source's new layout is known; moving the PMT PID can replay an
obsolete section under its new PID. Cover those source changes before
claiming readiness.

Public API: the handoff shape needs a maintainer decision. Wire: none.

## Waiting

- Maintainer: choose the program-map handoff for a replacement. Recommended:
  explicit provisional context in the Rust TS restart API, keeping
  `Restart::remainder` as unread source bytes and allowing the first real
  PAT/PMT to establish identity and layout. Alternative: keep the API and
  refactor importer/step ownership so source program-map packets remain
  with the media they describe; this is a larger internal change. Either
  must prevent a provisional PMT from advertising predecessor-only tracks.
  This changes API shape or step ownership beyond the original no-API-change
  expectation, so the implementation agent cannot settle it unasked.

## Closes

- [#7744](https://github.com/moq-dev/moq/issues/7744) - a republish is announced 1.2 s late and a stitched export loses a GOP
