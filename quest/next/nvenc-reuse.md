# [M] Reuse NVENC buffers after completion

## Goal

Repeated video encoding avoids unnecessary bitstream/input allocation and CUDA
registration while retaining every resource through its actual completion.

## Plan

The moq-video NVENC path allocates/destroys a bitstream per frame and creates an
input buffer or registration; CPU input also creates an interleaved UV buffer.
Measure those costs on representative CPU and CUDA input before choosing pool
depth. Reuse resources internally through the settled moq-nvenc ownership model.

Bound retained GPU memory, preserve device/context identity, and handle resize,
bitrate changes, delayed output, failure, and teardown. Do not keep a resource
registered against memory that its producer can overwrite or reclaim.

Test pool ownership and completion with the fake driver in CI; record hardware
latency, throughput, allocations, and registration counts separately. No public
pool controls are needed unless measurements establish a consumer requirement.
Public API and wire: unchanged.

## Required

- [NVENC resources](/quest/main/nvenc-resources.md) - reusable owned resources and completion contract

## Related

- [Hardware validation](/quest/future/video-hardware.md) - device-backed correctness evidence
