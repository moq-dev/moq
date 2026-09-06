# [L] Add moq-audio playback to the OBS source

## Goal

Subscribed MoQ audio plays through OBS alongside video using statically linked moq-audio codec code, including Opus, AAC-LC, and PCM. Audio playback can ship independently of video replacement and publishing.

## Plan

- The current source is video-only. Reuse `moq_consume_audio_raw` and its frame/free/close lifecycle, then feed converted PCM to `obs_source_output_audio`. Keep device playback inside OBS; do not enable moq-audio device capture/playback features or open a second audio device.
- Map catalog tracks, sample rates, channel layouts, and timestamps explicitly. Support mono/stereo initially, with explicit rejection or a tested OBS conversion for other layouts. Share the source's media timebase with video, preserve reconnect and rendition changes, and bound audio buffering. `latency_max_ms` controls stalled-group skipping, not desired A/V playout delay.
- Preserve OBS mixer, monitoring, mute, and volume behavior. Release every frame on output, conversion failure, stop, and late completion. Do not hold source state locks across callbacks into OBS.
- Verify audible output and recorded PCM, A/V synchronization with timestamped test media, rate changes, silence, stalls, reconnect, source replacement, and teardown. Exercise Opus, AAC-LC and PCM fixtures, not just callback counts. Update source documentation and Stats.

## Related

- [Video source replacement](/quest/m2/obs-moq-video/source.md) - coordinate the shared timestamp and source lifecycle without blocking audio rollout
