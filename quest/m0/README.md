# m0: Pronto GPU video

## Goal

Supply the reusable GPU media support needed to remove raw-pixel CPU transfers
from the Pronto CARLA demo on its Linux/NVIDIA desktop. This is the immediate
priority; the product integration and installation live in moq.pro.

## Quests

- [Vulkan/CUDA surfaces](/quest/m0/video-vulkan-cuda.md) - retain producer slots
  and synchronize GPU access safely across Vulkan and CUDA
- [NVENC registration rollback](/quest/m0/nvenc-registration.md) - release resources when mapping fails after registration
- [GPU conversion and NVENC](/quest/m0/video-gpu-encode.md) - convert, resize and
  encode imported frames without CPU pixel transfers or fallback

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/m0/pronto/gpu) - CARLA bridge, release adoption and desktop installation
