# m3: deferred

## Goal

Work whose first step is outside this repository: hardware nobody on the team
has, a partner or customer, or a hosting provider's offer. Nothing here can
start by opening an editor.

## Plan

A quest lands here when its gate is the outside world, not its priority. Each
states the condition in prose or as a plain-text `Required` bullet. When the
condition clears, move the quest to the milestone its work belongs in.

## Required

- [DPDK](/quest/m3/dpdk.md) - a kernel-bypass UDP path for the relay, once a provider offers SR-IOV or bare metal
- [Video hardware validation](/quest/m3/video-hardware.md) - run the encode, capture, and zero-copy paths that were written but never run on real machines
- [Validate PipeWire cameras on a portal and a Pi](/quest/m3/pipewire-camera-hardware.md) - run the shipped PipeWire camera through the camera portal and a Pi CSI node
- [#2893](/quest/m3/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - video: validate PipeWire DMA-BUF capture on KDE hardware
- [Embedded video](/quest/m3/video-embedded.md) - EGL import in the renderer, so moq-video presents on a Pi
- [Vision worker](/quest/m3/processor-vision.md) - a documented customer-run vision worker proves the processor contract
- [libmoq hidden opt-in](/quest/m3/libmoq-hidden.md) - `moq_origin_announced` takes a `hidden` flag so C callers can list `.`-named broadcasts
- [libmoq shutdown](/quest/m3/libmoq-shutdown.md) - OBS exits cleanly with the plugin loaded: a C ABI `moq_shutdown` stops the libmoq thread before the module is unloaded
- [libmoq CMake library](/quest/m3/libmoq-cmake-lib.md) - the in-tree CMake build links the `libmoq.a` cargo reports, not a hardcoded `target/<profile>` path
- [libmoq fetch](/quest/m3/libmoq-fetch.md) - libmoq gains an additive cached-group fetch entry point
- [Upstream forks](/quest/m3/upstream-forks.md) - offer the uniffi generator fixes our cpp, dart, and Python forks carry upstream, lowest priority
