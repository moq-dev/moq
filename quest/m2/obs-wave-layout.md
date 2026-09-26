# [XS] OBS channel layouts

## Goal

The OBS source maps a channel count to the same default layout moq-audio does,
the WAVE convention (3 is 2.1, 4 is quad, 6 is 5.1, 8 is 7.1), so a
multichannel broadcast plays with its speakers where the publisher put them.

## Plan

`audio_layout_to_speakers` in `cpp/obs/src/moq-source.cpp` maps from FFmpeg
layouts today. Map each count to the nearest OBS `speaker_layout` and refuse
the ones OBS cannot place rather than guess. Note the mapping in `doc/bin/obs.md`.

## Related

- [Audio codecs](/quest/m1/audio-codecs/README.md) - the channel layouts this mirrors
