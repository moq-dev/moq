# [M] Audio groups span 100 ms by default

## Goal

Every publisher that groups audio itself opens a new group (one QUIC stream)
every 100 ms instead of every packet, so a live audio track costs 10 streams a
second instead of 50. Each frame is still written as soon as it is encoded, so
latency does not change. Tiny audio streams stop racing each other and using up
`MAX_STREAMS` credit, which matters most behind the ~100-stream browser
WebTransport cap.

Not here: FEC or redundancy (a low-bitrate shadow copy of the previous group,
or Opus 1.5 DRED), datagram voice tracks, and aligning audio groups to video
GOPs. Audio muxed with video in fMP4 keeps following the video's segments.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan after a group-duration sweep):
✅ 200 ms (rejected: 1 s; keep 20 ms). The sweep ran moq-lite-07-wip over
moq-quic with bufferbloat and `MAX_STREAMS = 100`, and replayed the arrival
traces through `@moq/watch`'s audio path:

- 8 speakers plus 2 videos through a 1.5 s collapse: 20 ms groups caused 35
  `STREAMS_BLOCKED_UNI`, 274 resets, and 65% more bottleneck drops. 200 ms
  caused none blocked and 36 resets.
- At 160 ms RTT, 20 ms groups dropped 49 frames, because newer groups
  superseded a frame before its retransmit landed. 200 ms dropped 15.
- 1 s saves only 4 more streams a second. One lost group became a 960 ms
  gap, and a late join replays about 550 ms of backlog.

Decided 2026-11-25 (maintainer, /quest-run): ✅ 100 ms, aligned with the
native PLC's ~100 ms concealment cap
([moq-dev/moq#9562](https://github.com/moq-dev/moq/pull/9562)), so a lost
group is concealed rather than half silent (rejected: 200 ms, the 11-24 pick;
1 s; keep 20 ms). It costs 10 streams a second instead of 5.

Publishers to change (as of 47e3a57a7b):

- `moq_audio::encode::Options::join` (`rs/moq-audio/src/encode/producer.rs`):
  change the default to 100 ms. moq-ffi and the SDKs built on it inherit it.
- `@moq/publish`'s `Encoder.join` (`js/publish/src/audio/encoder.ts`): change
  the default to 100 ms. Also close a group on the frame that fills it, as Rust
  does in `publish`. JS today holds the group open until the next frame
  arrives, so the relay sees the group finish one frame late.
- moq-webrtc's Opus bridge (`rs/moq-webrtc/src/codec/opus.rs`) cuts after
  every packet. Its comment is wrong: the importer writes each frame
  immediately, so the cut only closes the group. Cut once per join interval
  instead. Don't just delete the cut, or the group never closes.
- The audio-only fMP4 import (`starts_segment` in
  `rs/moq-mux/src/container/fmp4/import.rs`) opens a group per fragment.
  `-frag_duration 1000`, as `test/audio-quality/run.sh` and
  `demo/pub/bbb.py` pass to ffmpeg, makes that a group per Opus frame. Roll
  audio-only groups by duration instead.

Recommendation: keep one 100 ms default in moq-mux that each Rust publisher
reads, and mirror it in JS, rather than repeating the constant. Audit other
audio publishers (gstreamer, OBS, Python) for their own per-packet grouping.
The catalog already declares the join interval (`groupSpacing`), so players
need no change. Check that `Start::Latest` joins still reach the live edge in
`delay.min` plus warmup.

Validation: run `just test audio-quality` and `just bench-audio` before and
after, and record underruns, trimmed audio, recovery, stream counts, and
relay CPU in the PR. bench-audio already sweeps 20, 100, and 200 ms groups.
With these changes the harness's Opus row publishes 100 ms groups.

Public API: none (a default changes). Wire: none.

## Related

- [Buffered playback subscribes in ascending order](/quest/m3/buffered-ascending.md) - live audio stays newest-first even with 100 ms groups
- [An audio track's final flush joins its open group](/quest/m4/audio-flush-group.md) - the same race at every group boundary, now 10x rarer
- [Native audio conceals short losses](/quest/m1/audio-plc.md) - covers part of a lost group, which is now 100 ms
