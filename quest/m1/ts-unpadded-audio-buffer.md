# [S] An unpadded TS export keeps MP2 audio within its decoder buffer

## Goal

`moq export ts` of a variable-rate source carrying MP2 audio, exported unpadded, passes the
harness's T-STD model: B never overflows.

## Plan

Found while validating [moq-dev/moq#9636](https://github.com/moq-dev/moq/pull/9636), on its base
commit: `just test ts --source vbr_mp2.ts` (FFmpeg, 320x240 H.264 at 300 kb/s and stereo MP2 at
96 kb/s, no `-muxrate`) fails `tstd` with `PID 257 B overflow x23` at a 104.9% peak. A padded
export of the same audio keeps B under 99%. Generate the source with:

```bash
ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25 -f lavfi -i "anoisesrc=d=20:c=pink:r=48000:a=0.5:seed=7" -t 20 -c:v libx264 -g 50 -b:v 300k -c:a mp2 -b:a 96k -ac 2 -f mpegts vbr_mp2.ts
```

The unpadded schedule counts B occupancy without an exact packet clock (`Schedule::admit`), while
the analyzer times each packet between the PCRs around it; MP2 packs a PES as full as B allows
(about 12 frames), so a packet that the analyzer places before the first frame leaves overflows
it. Find which side's timing is wrong before changing either.
