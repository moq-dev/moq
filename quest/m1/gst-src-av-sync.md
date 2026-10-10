# [M] moqsrc keeps audio and video aligned

## Goal

All of a `moqsrc` run's pads share one timestamp reference and one segment
base, so audio and video keep the relative timing the broadcast gives them,
at the first start and after every switch. Today each pump starts its PTS at
its own first frame (`reference_ts` in `rs/moq-gst/src/source/imp.rs`) and
bases its segment on the running time its own first buffer arrived
(`live_segment`), so two tracks line up only as well as their first frames'
arrivals happen to match their timestamps. A video track joining at a
keyframe and an audio track joining at its latest group start apart.

Non-goals: aligning to wall time across hosts, which
[#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md)
settles on the sink side; jitter buffering.

## Plan

Decided 2026-10-10, planning the follow-ups of
[#5181](https://github.com/moq-dev/moq/pull/5181): the tracks of one
broadcast share a microsecond timeline (`hang::container::TIMESCALE`, which
the catalog's one `clock` maps to wall time), so equal timestamps on
different tracks present together. `moqsink` already publishes them that
way (`two_pads_keep_av_aligned_through_real_segments`); only `moqsrc` loses
it.

Open, for the maintainer:

- The shared reference. Recommended: per run, the first frame any pump reads
  fixes the run's reference timestamp and segment base, and every pad of the
  run maps through both, so a frame stamped before the reference falls
  before the segment and is clipped. Alternative: map timestamps through the
  catalog's wall `clock` onto the pipeline clock, which also aligns across
  hosts but assumes synchronized clocks and overlaps #3021.
- Milestone: m1 is the recommendation; m2 is the alternative.
- Docs: whether anything beyond the `moqsrc` paragraph of
  `doc/bin/gstreamer.md` is needed is open; recommended no.

Test: a broadcast whose audio and video first frames differ by a known
offset reaches `moqsrc`'s two pads with that offset between their running
times, at the first start and after a restart.

## Required

- [moqsrc follows a restart](/quest/m0/broadcast-epoch/moqsrc.md) - rewrites the pumps and segments this changes

## Related

- [#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - the wall epoch on the sink side
