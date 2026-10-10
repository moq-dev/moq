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
broadcast share one timeline, which the catalog's one `clock` maps to wall
time, so equal times on different tracks present together. They do not
share one scale: CMAF keeps each track's `mdhd` timescale, and LOC can carry
a per-frame one (`moq_mux::container::Frame::timestamp`). `moqsink` already
publishes aligned tracks (`two_pads_keep_av_aligned_through_real_segments`);
only `moqsrc` loses the alignment.

Whatever the reference:

- Compare times converted to one scale, such as nanoseconds:
  `moq_net::Timestamp::checked_sub` refuses mismatched scales, and
  `relative_pts` turns that refusal into zero.
- A frame before the run's reference must land before the segment start,
  where the segment clips it. Today `relative_pts` clamps it to zero instead.
  For example, use the converted time as the PTS and the reference as the
  segment start.

Open, for the maintainer:

- The shared reference. Recommended: per run, the first frame any pump reads
  fixes the run's reference timestamp and segment base, and every pad of the
  run maps through both. Alternative: map timestamps through the
  catalog's wall `clock` onto the pipeline clock, which also aligns across
  hosts but assumes synchronized clocks and overlaps #3021.
- Milestone: m1 is the recommendation; m2 is the alternative.
- Docs: whether anything beyond the `moqsrc` paragraph of
  `doc/bin/gstreamer.md` is needed is open; recommended no.

Test: a broadcast whose audio and video first frames differ by a known
offset reaches `moqsrc`'s two pads with that offset between their running
times, at the first start and after a restart, whichever pump reads first.
One case is a CMAF broadcast whose tracks use different timescales.

## Required

- [moqsrc follows a restart](/quest/m0/broadcast-epoch/moqsrc.md) - rewrites the pumps and segments this changes

## Related

- [#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - the wall epoch on the sink side
