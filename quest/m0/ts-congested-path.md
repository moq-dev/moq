# [M] The scheduled TS congested path keeps its audio

## Goal

The `TS congested path` step of the scheduled `TS` workflow
(`just test ts --headroom --rate 7000000 --duration 120`) passes on `main`
again, fixed at its cause: the export sheds whole video groups, and keeps
every audio frame from the join on.

## Plan

It passed on the 2026-11-21 run at `d1c532d3e4` and failed on the next two:

- [45040473444](https://github.com/moq-dev/moq/actions/runs/45040473444) at
  `f2eab6d3c2`: each audio PID shed frames (MP2 PIDs 4, 12 and 37 frames,
  AC-3 2), which `holes.py` fails since only video may shed.
- [45251919956](https://github.com/moq-dev/moq/actions/runs/45251919956) at
  `a04b99fbc9`: no audio shed, but each audio PID starts 3.008 to 3.024 s
  after its source (at 4.49 s of 1.47 s), one frame past the 3.0 s join
  allowance. Video shed 67.4 s of 117 s through `current group evicted;
  skipping to next buffered group` every second.

The moq-mux and `test/ts` commits between `d1c532d3e4` and `f2eab6d3c2`
include `3d1bb40f2a` (a live join asked upstream per join,
[moq-dev/moq#9316](https://github.com/moq-dev/moq/pull/9316)), `c145046a74`
(lite-07 group streams) and `a748733392` (group presentation order), each a
candidate for the later audio start. Reproduce locally through `quest gate`
and bisect that range before changing anything; neither run's failure was
rerun to see whether it repeats.
