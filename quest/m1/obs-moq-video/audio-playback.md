# [L] Replace the OBS source's FFmpeg audio decode with moq-audio

## Goal

The MoQ source decodes subscribed audio with statically linked moq-audio instead of libavcodec, covering Opus, AAC-LC, and PCM, and keeps its multichannel speaker placement. Audio can ship independently of video replacement and publishing.

## Plan

- Today the source already plays audio: `moq_source_subscribe_audio` in `cpp/obs/src/moq-source.cpp` receives encoded frames through libmoq's `moq_consume_audio`, and `moq_source_decode_audio_frame` decodes them with libavcodec before `obs_source_output_audio`. Replace that decode, not the playback path.
- Decode through the generated C++ package with `MoqBroadcastConsumer::decode_audio` (a `MoqAudioConsumer` with a `next()` future per frame, cancel on drop), then feed its PCM to `obs_source_output_audio`. `decode_audio` accepts Opus and AAC-LC today; add PCM there rather than in the plugin. Keep device playback inside OBS; do not enable moq-audio device capture/playback features or open a second audio device.
- Map catalog tracks, sample rates, channel layouts, and timestamps explicitly. Channel layouts: map each channel count to the default layout moq-audio uses, the WAVE convention (3 is 2.1, 4 is quad, 6 is 5.1, 8 is 7.1), picking the nearest OBS `speaker_layout`, and refuse counts OBS cannot place rather than guess. This replaces `audio_layout_to_speakers`, which maps from FFmpeg layouts, and absorbs the former m2 obs-wave-layout quest. Note the mapping in `doc/bin/obs.md`.
- Share the source's media timebase with video, preserve reconnect and rendition changes, and bound audio buffering. `latency_max_ms` controls stalled-group skipping, not desired A/V playout delay.
- Preserve OBS mixer, monitoring, mute, and volume behavior. Release every frame on output, conversion failure, stop, and late completion. Do not hold source state locks across callbacks into OBS.
- Remove the audio FFmpeg includes and codec mapping. Whichever of this and the video source replacement lands second removes the remaining FFmpeg CMake linkage.
- Verify audible output and recorded PCM, A/V synchronization with timestamped test media, rate changes, silence, stalls, reconnect, source replacement, teardown, and a 5.1 fixture placed on the right speakers. Exercise Opus, AAC-LC and PCM fixtures, not just callback counts. Update source documentation and Stats.

Decided: the earlier receive branch (#3498, merged only into the stranded `codex/obs-audio-receive-base`) is abandoned rather than rebased; this quest starts from the plugin on the generated C++.

## Required

- [OBS migration](/quest/m1/cpp/obs.md) - the plugin is on the generated C++ first

## Related

- [Video source replacement](/quest/m1/obs-moq-video/source.md) - coordinate the shared timestamp and source lifecycle without blocking audio rollout
- [Channel layouts](https://github.com/moq-dev/moq/pull/4119) - the moq-audio layouts this mapping mirrors
