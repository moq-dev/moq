# [S] A passthrough TS import splices a clock step instead of ending

## Goal

`moq import ts --passthrough` keeps its broadcast across every PCR step that
is not elapsed time: an unflagged step back, a flagged step back, and an
unflagged step forward of more than 10 s. Its `moq export ts --passthrough`
subscribers keep playing, and the bytes they get stay identical to the
input. `PcrRewind` and `PcrRestart` are gone.

## Plan

Decided 2026-11-17 (maintainer, `/quest-import` of
[moq-dev/moq#8911](https://github.com/moq-dev/moq/issues/8911)):

- ✅ Splice in place (rejected: won't fix, leaving the source to flag
  the switch or lock its encoders; republish under a new instance on the
  import and teach the passthrough release to follow it; republish on the
  import only). The passthrough promises byte-identical carriage, as an
  SRT hop does, and decoders ride through the step. It reads no PES, its
  catalog describes no codecs, and its one reader (`ts::Release`) already
  re-anchors its jitter buffer on a step. The reasons the media-aware import
  republishes ([moq-dev/moq#7144](https://github.com/moq-dev/moq/pull/7144):
  it rewrites PES timestamps and describes codecs) do not apply. Republishing
  would also drop the input until the new source's PAT, PMT, and keyframe,
  and a stitched passthrough output would carry the same unflagged step.
- ✅ Splice every step that is not elapsed time, forward included
  (rejected: back only). Today an unflagged forward step under half the
  wrap is added to the timeline with no marker
  (`rs/moq-mux/src/container/ts/passthrough.rs`, `ticks = last + step`), so
  switching back between the reported encoders would push the timeline
  4.7 h past the wall clock. Use the media-aware judge's 10 s bound
  (`LEAP`, `rs/moq-mux/src/container/ts/step.rs`).
- ✅ m2 (rejected: m1). The fix deletes two public error variants, and
  nothing here is on `release` to backport.

The splice rule: at such a PCR, time the bytes before it at the rate before
it, continue the count from there (raised to at least the last written
time), and start the next group behind hang's discontinuity marker, as the
flagged forward jump already does. It depends only on the bytes, so two
publishers starting at the same packet still publish identical frames.
Amend `drafts/draft-lcurley-moq-mpegts.md`'s Timestamps section ("a PCR
that steps backwards MUST end the track") and `doc/bin/ts.md`. The
passthrough's module comment ("a signalled restart is new content and
needs a new broadcast") goes with it.

The reported step is real: it is the feed from
[moq-dev/moq#7102](https://github.com/moq-dev/moq/issues/7102), two
encoders 16,844.5 s apart behind a switch that sets no
`discontinuity_indicator`. The logged PCRs (1,642 s → 80,241 s) are that
step back across the 33-bit wrap, not an overflow.

A single corrupt PCR now costs two splices instead of the broadcast.

Tests: an unflagged step back, a flagged step back, and an unflagged step
forward past 10 s, each at a GOP boundary and mid-GOP: the import keeps
running, timestamps keep rising, the output bytes equal the input, and the
release plays through without holding. The reporter offered to run the
branch on the same feed.

Public API: breaking, `moq_mux::Error::PcrRewind` and `PcrRestart` are
removed. Wire: the MPEG-TS draft's passthrough timestamp rule changes; the
existing reader handles the result.

## Closes

- [#8911](https://github.com/moq-dev/moq/issues/8911) - `import ts --passthrough` ends at an unflagged clock step back

## Related

- [A transport stream carried whole is listed under the mpegts section](/quest/m2/hang04/m2ts.md) - also edits the passthrough import and release
