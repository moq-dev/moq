# [L] moq-audio filter chain

## Goal

Each audio source can run an ordered processing chain in moq-audio before the
program bus: noise suppression, noise gate, compressor, and limiter, plus
ducking, which lowers one source (music) when another (the microphone) is
active. Third-party VST3 and CLAP plugins load into the chain. The
[studio's audio filters](https://github.com/moq-dev/studio/blob/main/quest/m0/audio-filters.md) in moq-dev/studio
configure it per source.

## Plan

Changed 2026-11-02 (maintainer): the studio moved to moq-dev/studio. The
chain lands here with its own tests and benchmark; the studio's panel and
scene-file shape follow a pin bump there.

Decided 2026-11-02 (maintainer): the chain runs on the capture producer
side of [one capture, many consumers](/quest/m1/audio-capture-fanout.md), so
the bus and a source's own track both carry filtered audio; gain and mute
stay after the split, in the bus `Strip`, so the chain has no gain stage of
its own; a compressor's makeup gain covers level fixes.

Prefer maintained crates, such as `nnnoiseless` for RNNoise suppression, over
hand-rolled DSP. Benchmark the chain per source so its cost per added source
shows up as a slope. Host plugins out of process, so a crashing or hung plugin
drops that filter rather than the audio bus: the audio thread never blocks on
the plugin process, a block the plugin misses its deadline for passes through
unfiltered, and a plugin that keeps missing is bypassed and reported as failed.
Use a maintained host crate (such as `clack` for CLAP) rather than
hand-rolling the plugin ABIs.
