# [S] TS export survives a content restart on a continuous timeline

## Goal

`moq export ts` keeps emitting every elementary stream across a content join
on a source whose transport timeline is continuous: PCR, PTS and DTS monotone
on the wire, continuity counters unbroken, no `discontinuity_indicator`. That is
what a real encoder produces at a hard cut. The only backwards step in that
scenario is the importer's: the legacy audio importer extrapolates timestamps
from the last PES header, so after a resync at the join it re-locks a frame a
few milliseconds below its own extrapolated high-water mark. A rewind detected
on one track may cost the program one clock and PSI reset, but it never fences
another track for good, and the true rewind recovery #3375 added, a looping
file where every track steps backwards, keeps withholding stale frames.

Boundaries: the consumer's rewind detection and the legacy audio importer's
resync are untouched here. Retiring inferred rewinds altogether is
[Monotonic timeline](/quest/m1/monotonic-timeline.md).

## Plan

Since #3375, `rewind(backwards)` in `rs/moq-mux/src/container/ts/export.rs`
bumps the program epoch and, on a backwards boundary, leaves every track that
already has a timeline in the old epoch. `Track::admit` then discards that
track's frames until it both changes its discontinuity counter and steps below
its own high-water mark. A continuous source supplies neither. The consumer's
rewind check in `rs/moq-mux/src/container/consumer.rs` has no tolerance, so the
sub-frame backwards step the legacy importer produces on an MPEG-1 audio resync
at the join is read as a rewind, and video plus primary audio are fenced
permanently while the passthrough PSI, AC-3 and teletext continue. #3533
measured 0.31 Mb/s against a 9.5 Mb/s source with no recovery over 40 minutes,
bisected to #3375, and showed a single-track source is immune because the fence
needs a bystander.

- Give the fence an exit. A fenced track joins the new generation when its own
  timeline steps back, as today, or when the program clock driven by the joined
  tracks passes its pending frame's timestamp. On a true rewind the video
  track's own boundary arrives long before the reset clock climbs back to its
  stale frames, so they are still discarded, which is what
  `rewind_flags_the_break_once_across_tracks` asserts. On the #3533 join the
  reset clock sits a few milliseconds below video's next frame, so video
  re-joins within one frame. Implement it in `Track::admit` against the
  exporter's watermark rather than as a timer: the exit is a clock comparison,
  never a deadline. A frame at or below the watermark is admitted, a frame
  above it stays fenced, and `rewind()` clears the watermark, so nothing
  re-joins until a joined track has emitted; `fill` passes the current
  watermark into admission so the rule has one definition.
- Regression test in `export_test.rs`: two tracks on a continuous transport
  timeline whose content restarts, the audio track alone stepping back by less
  than one frame at the join; video and audio keep emitting across it, and the
  join costs at most one PCR discontinuity and one PSI re-emission. The existing
  `rewind_re_emits_tables_and_resumes_the_clock` and
  `rewind_flags_the_break_once_across_tracks` keep passing unchanged.

## Closes

- [#3533](https://github.com/moq-dev/moq/issues/3533) - close this issue when the quest finishes

## Related

- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - deletes the inferred rewind that triggers the false boundary
- [Gap discontinuity](/quest/m1/monotonic-timeline.md) - the declared-break model the exporter will read instead
