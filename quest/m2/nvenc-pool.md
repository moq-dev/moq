# [M] NVENC reuses its input and output buffers

## Goal

The NVENC backend stops allocating per frame. At 1080p, creating the output
bitstream and input buffer (or registering the CUDA resource) is about half
of frame-to-packet time. A benchmark shows the pooled path is faster at
720p and 1080p, with the numbers in the PR. If it doesn't win, abandon the
quest and report the numbers.

## Plan

`rs/moq-video/src/encode/backend/nvenc.rs` calls `create_output_bitstream`,
`create_input_buffer` (CPU frames) or `register_generic_resource` (CUDA
frames) on every `encode`, and frees them all at the end of the call. Keep a
small pool sized by the frames in flight, which is one today since B-frames
are off. Re-register a CUDA resource only when its pointer changes. Measure
with the `encode-presets` example from #4099.
