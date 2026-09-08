# [S] TS export survives a content restart on a continuous timeline

## Goal

`moq export ts` keeps emitting every elementary stream across a content join
on a source whose timeline is continuous: PCR, PTS and DTS monotone, continuity
counters unbroken, no `discontinuity_indicator`. That is what a real encoder
produces at a hard cut. A rewind detected on one track may cost the program one
clock and PSI reset, but it never fences another track for good. The true
rewind recovery #3375 added, a looping file where every track steps backwards,
keeps working.

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

- Every track joins the new epoch on every rewind: delete the `backwards`
  branch in `rewind()`, its parameter, and the epoch test in `Track::admit`,
  keeping the watermark, clock, counter, PSI and PCR reset that #3375 introduced
  for #2833. A bystander that still holds media from before the boundary emits
  it under the reset clock instead of being discarded, and its own next
  boundary resets again. Delete whatever only the fence kept alive.
- Regression test in `export_test.rs`: two tracks on a continuous timeline
  whose content restarts, the audio track alone stepping back by less than one
  frame at the join; video and audio keep emitting across it, and the join
  costs at most one PCR discontinuity and one PSI re-emission. The existing
  `rewind_re_emits_tables_and_resumes_the_clock` and
  `rewind_flags_the_break_once_across_tracks` keep passing.
- Land after [TS timebase discontinuity](/quest/m0/ts-forward-discontinuity.md)
  (PR #3529), which edits the same functions and makes the legacy importer
  declare its breaks.

## Required

- [TS timebase discontinuity](/quest/m0/ts-forward-discontinuity.md) - PR #3529 rewrites the same boundary handling; rebase on it rather than race it

## Closes

- [#3533](https://github.com/moq-dev/moq/issues/3533) - close this issue when the quest finishes

## Related

- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - deletes the inferred rewind that triggers the false boundary
- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - the declared-break model the exporter will read instead
