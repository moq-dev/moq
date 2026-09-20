# [L] GPU color conversion and NVENC for imported frames

## Goal

moq-video converts imported packed RGB frames to encoder input, resizes for
multiple renditions and produces timestamped H.264 packets through NVENC,
without raw pixels crossing CPU memory. The GPU path fails explicitly when
unsupported and never selects a CPU conversion, upload or software encoder.

## Plan

- Add GPU color conversion for the packed BGRA/RGBA input the CARLA bridge
  needs, and reuse GPU resize and NVENC support. Specify channel order,
  color matrix, range, chroma sampling, pitch and dimensions; match the
  producer's final LDR output without applying gamma twice.
- Keep input and converted surfaces owned through conversion and encoder
  completion. Support one captured frame feeding HD and SD with bounded GPU
  resources; encoder pressure cannot grow a queue indefinitely.
- Audit every operation used by this path for implicit CPU conversion or
  download. Existing GPU resize and encoder defaults are not proof of strict
  GPU execution. Provide explicit unsupported/error behavior through a small
  coherent API instead of changing unrelated CPU consumers.
- Choose supported NVENC input registration after checking current bindings
  and driver support. Existing CUDA NV12 allocations provide a starting point;
  compare newer CUDA-array/stream support before adding another GPU staging
  copy. Do not require a speculative SDK upgrade to finish the feature.
- Preserve explicit timestamps, force-IDR requests and codec configuration.
  Expose compressed packets and completion through the existing encoder
  abstractions so a narrow native CARLA bridge can use them.
- Exercise actual Vulkan input, GPU conversion/resize and NVENC on this
  desktop. Check channel/color correctness, frame identity, timestamp order,
  IDR restart, repeated pool reuse and teardown with a decoder. Keep test-only
  reference readbacks outside the supported runtime path. Trace that runtime
  to prove no raw-pixel CPU transfers, and record copies, CPU time and latency
  without numeric performance acceptance thresholds. Wire regression and
  opt-in hardware tests into the repository's normal test commands.

## Required

- [NVENC registration rollback](/quest/m0/nvenc-registration.md) - safe cleanup when input mapping fails

- [Vulkan/CUDA surfaces](/quest/m0/video-vulkan-cuda.md) - owned input and GPU synchronization

## Related

- [Video hardware validation](/quest/m4/video-hardware.md) - prior NVENC allocation findings and separate hardware coverage
- [NVENC programming guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/) - registration and input lifetime requirements
- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/m0/pronto/gpu) - the first consumer and desktop measurements
