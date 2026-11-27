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

# FFmpeg-packed AAC

`ffmpeg_aac.ts` is five seconds of stereo 44.1 kHz AAC at 128 kb/s with FFmpeg's default MPEG-TS
packing: a PES of about 186 ms in each of its audio packets, a clock in the first packet of each, and
no null packets, so its recorded multiplex rate is the audio's own. Generated in the repository Nix shell with:

```bash
ffmpeg -hide_banner -loglevel error -f lavfi -i "anoisesrc=d=5:c=pink:r=44100:a=0.5:seed=7" -af tremolo=f=1.5:d=0.9 -ac 2 -c:a aac -b:a 128k -f mpegts ffmpeg_aac.ts
```

The other `ffmpeg_*.ts` are four seconds of stereo audio packed the same way, each a codec the
export carries: AC-3, E-AC-3 and MP2 at 96 kb/s, Opus at 64 kb/s, and variable-rate AAC pulsed so
its frames peak at over twice their average. Generated in the repository Nix shell with:

```bash
SRC="anoisesrc=d=4:c=pink:r=48000:a=0.5:seed=7"
for codec in ac3 eac3 mp2; do
  ffmpeg -hide_banner -loglevel error -f lavfi -i "$SRC" -af tremolo=f=1.5:d=0.9 -ac 2 -c:a $codec -b:a 96k -f mpegts ffmpeg_$codec.ts
done
ffmpeg -hide_banner -loglevel error -f lavfi -i "$SRC" -af tremolo=f=1.5:d=0.9 -ac 2 -c:a libopus -b:a 64k -f mpegts ffmpeg_opus.ts
ffmpeg -hide_banner -loglevel error -f lavfi -i "anoisesrc=d=4:c=white:r=44100:a=0.8:seed=7" -af "volume=if(lt(mod(t\,0.5)\,0.1)\,1\,0.001):eval=frame" -ac 2 -c:a aac -q:a 1 -f mpegts ffmpeg_aac_pulse.ts
```
