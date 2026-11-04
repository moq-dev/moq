# POC fixtures

`poc-avc.ts` and `poc-hevc.ts` contain twelve 32x32 pictures at 25 fps,
including B pictures. The original PES PTS values are an independent oracle for
repartitioning the identical elementary bytes into single, combined, and split
PES, then exporting and reimporting them. SEI is removed here because the test
uses the declared proportional POC clock, not HRD picture timing. T-STD's
separate generated controls retain HRD to prove decode-time derivation too.

Generated in the repository Nix shell with:

```bash
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=32x32:rate=25 -frames:v 12 -an -c:v libx264 -x264-params bframes=3:force-cfr=1:keyint=12:aud=1:threads=1 -bsf:v filter_units=remove_types=6,h264_metadata=fixed_frame_rate_flag=1 -f mpegts poc-avc.ts
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=32x32:rate=25 -frames:v 12 -an -c:v libx265 -x265-params bframes=3:keyint=12:aud=1:pools=none:frame-threads=1 -bsf:v filter_units=remove_types=39,hevc_metadata=tick_rate=25:num_ticks_poc_diff_one=1 -f mpegts poc-hevc.ts
```

The test adds AVC's MPEG-TS timing descriptor to its repacketized PMT: fixed
frame rate plus temporal POC explicitly permits scaling POC by the VUI tick.
