# [L] Retained Vulkan/CUDA video surfaces

## Goal

A Linux/NVIDIA producer can hand a Vulkan image to moq-video for GPU consumption
without raw pixels touching CPU memory, premature slot reuse, or leaked GPU
resources. Exercise the contract on the Pronto desktop's GPU using a native
Vulkan producer independently of Unreal integration.

## Plan

- Extend the existing frame/surface ownership model with the minimum reusable
  import capability the CARLA consumer needs. Allocation ownership must also
  retain the producer's slot until all GPU readers complete; keeping an image
  or fd alive alone does not prevent overwrite.
- Establish same-device matching, exportable allocation creation, image format
  and layout, Vulkan/CUDA access ordering, and completion-driven slot return.
  Unreal's native VkImage and bGPUSharedFlag do not establish exportability.
  Allow a GPU copy into an owned exportable slot when direct sharing is not
  supported. Document copies rather than calling every GPU path zero-copy.
- Use explicit GPU synchronization and bounded in-flight resources. Completion
  must progress when capture stops. Handle cancellation, resize, producer
  destruction, initialization failure and device loss without CPU polling loops
  or blocking the producer's render thread for each frame.
- Refuse unsupported devices, formats and synchronization mechanisms. This
  contract offers no raw-pixel CPU mapping, download or staging fallback.
- Add deterministic ownership/error regression tests to the normal gates and
  an opt-in hardware exercise reachable through the repository test commands.
  Run it on this desktop with GPU-produced frame identities across repeated
  slot reuse, held consumers, cancellation and teardown. Record the exact
  GPU/driver and trace resource lifetimes and pixel transfers. A capability
  declaration or compile-only result is insufficient evidence of working import.

## Related

- [GPU conversion and NVENC](/quest/main/video-gpu-encode.md) - consumes the imported surfaces
- [PipeWire surface lifetime](/quest/future/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - reuse ownership principles without taking on PipeWire completion
- [Linux OBS GPU export](/quest/future/obs-linux-gpu.md) - another potential consumer; OBS and VAAPI remain separate
- [CUDA graphics interoperability](https://docs.nvidia.com/cuda/cuda-programming-guide/04-special-topics/graphics-interop.html) - external memory and semaphore contracts
