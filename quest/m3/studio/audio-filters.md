# [M] Audio filters

## Goal

Each audio source has an ordered processing chain in moq-audio: gain, noise
suppression, noise gate, compressor, and limiter. Each filter is configured
from the studio and saved with the scene collection. Ducking lowers one source
(music) when another (the microphone) is active. Third-party VST3 and CLAP
plugins load into the chain.

## Plan

Prefer maintained crates, such as `nnnoiseless` for RNNoise suppression, over
hand-rolled DSP. Benchmark the chain per source so its cost per added source
shows up as a slope. Host plugins out of process, so a crashing or hung plugin
drops that filter rather than the audio bus, and use a maintained host crate
(such as `clack` for CLAP) rather than hand-rolling the plugin ABIs.
