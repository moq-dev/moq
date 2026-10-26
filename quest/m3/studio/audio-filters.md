# [M] Audio filters

## Goal

Each audio source has an ordered processing chain in moq-audio: gain, noise
suppression, noise gate, compressor, and limiter. Each filter is configured
from the studio and saved with the scene collection.

## Plan

Prefer maintained crates, such as `nnnoiseless` for RNNoise suppression, over
hand-rolled DSP. Benchmark the chain per source so its cost per added source
shows up as a slope.
