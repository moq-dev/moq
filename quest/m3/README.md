# m3: deferred

## Goal

Work whose first step is outside this repository: hardware nobody on the team
has, a partner or customer, or a hosting provider's offer. Nothing here can
start by opening an editor.

## Plan

A quest lands here when its gate is the outside world, not its priority. Each
states the condition in prose or as a plain-text `Required` bullet. When the
condition clears, move the quest to the milestone its work belongs in.

## Quests

- [DPDK](/quest/m3/dpdk.md) - a kernel-bypass UDP path for the relay, once a provider offers SR-IOV or bare metal
- [Video hardware validation](/quest/m3/video-hardware.md) - run the encode, capture, and zero-copy paths that were written but never run on real machines
- [Validate PipeWire cameras on a portal and a Pi](/quest/m3/pipewire-camera-hardware.md) - run the shipped PipeWire camera through the camera portal and a Pi CSI node
- [#2893](/quest/m3/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - video: validate PipeWire DMA-BUF capture on KDE hardware
- [Embedded video](/quest/m3/video-embedded.md) - EGL import in the renderer, so moq-video presents on a Pi
- [Vision worker](/quest/m3/processor-vision.md) - a documented customer-run vision worker proves the processor contract
