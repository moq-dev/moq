# [S] GPU color conversion and NVENC for imported frames

## Goal

moq-video converts imported packed RGB frames to encoder input, resizes for
multiple renditions and produces timestamped H.264 packets through NVENC,
without raw pixels crossing CPU memory. The GPU path fails explicitly when
unsupported and never selects a CPU conversion, upload or software encoder.

## Plan

The code is in: `frame::cuda::Converter` converts a published
`frame::vulkan::Frame` (RGBA8 or BGRA8, declared on `vulkan::Image`) to an
NV12 `cuda::Frame` on the GPU in a declared color space, from a bounded buffer
pool; `cuda::Frame::resize` scales it there from the same pool; NVENC
registers the result in place through the existing `encode::Encoder` with
`Kind::Named("nvenc")`. The module docs record the audit and the registration
choice. What remains needs the Linux/NVIDIA desktop:

- Run `just rs vulkan-cuda`, which now also exercises conversion, resize, the
  pool bound and NVENC through `frame::cuda::tests::vulkan_cuda_convert_resize_encode`.
  It checks channel order, matrix and range against a CPU reference, frame
  identity through a decoder, timestamp order, forced and periodic IDR,
  repeated slot and pool reuse, and teardown. Fix what it finds; the PTX and
  the kernel launch have never run on a GPU.
- Trace that runtime (an `nsys` or CUDA API trace of the test, or of the CARLA
  bridge once it exists) to prove no `cuMemcpyDtoH`/`HtoD` of raw pixels on the
  supported path; the only host copies must be the test's own readbacks.
- Record copies, CPU time and latency per stage at the target workload (three
  1280x720 views at 30 fps) without numeric acceptance thresholds, and note
  whether the per-frame NVENC register/unregister is worth replacing with a
  registration per pool buffer.

## Related

- [Video hardware validation](/quest/future/video-hardware.md) - prior NVENC allocation findings and separate hardware coverage
- [NVENC programming guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/) - registration and input lifetime requirements
- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/main/pronto/gpu) - the first consumer and desktop measurements
