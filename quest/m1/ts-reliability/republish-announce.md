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

Decided 2026-11-04 (maintainer, /quest-unblock): how a replacement gets the
program map that already routed before the step.

- ✅ Explicit provisional context in the Rust TS restart API, keeping
  `Restart::remainder` as unread source bytes. The first actual PAT/PMT
  establishes the new identity and layout, and predecessor-only tracks must
  not announce.
- Not chosen: preserve the API and refactor importer/step ownership so
  program-map packets stay with their media.

Replace the draft's synthetic PAT/PMT prefix with that context. Cover a
source whose PCR precedes its new PAT under another identity, a moved PMT
PID, and a PMT-configured track only the predecessor listed.

Public API: `Restart` gains the provisional map, which a new `Import`
follows until the source's own tables arrive. Wire: none.

## Closes

- [#7744](https://github.com/moq-dev/moq/issues/7744) - a republish is announced 1.2 s late and a stitched export loses a GOP
