# OBS encoding with moq-video

## Goal

OBS can encode its composited video with moq-video, with explicit low-latency configuration and a GPU path that avoids downloading every frame to the CPU. Keep OBS audio encoding and the current publication lifecycle working during rollout.

## Plan

The current dock creates OBS video encoders in `cpp/obs/src/moq-dock.cpp`; `MoQOutput::EncodedPacket` publishes their encoded packets. Registering a thin OBS video encoder backed by moq-video is the preferred starting point: OBS still supplies its composited frames, while moq-video owns codec policy. Compare this with a raw-output implementation before committing to the interface. OBS's encoded-output flag applies to the output, so bypassing its encoder interface must explain how encoded audio remains supported.

`rs/libmoq/src/video.rs` already publishes CPU I420/RGBA through moq-video, but its frame call copies before returning and couples encoding to publication. It is a baseline, not a GPU import API or an encoded-packet adapter. `rs/moq-video/src/frame.rs` already models PixelBuffer, D3D11 Texture, and DMA-BUF surfaces. Reuse these representations where possible; audit which backends actually accept the OBS format and device.

Define zero-copy precisely in results: direct surface reuse, GPU conversion/blit without CPU readback, and CPU staging are different paths. A bounded GPU blit is an acceptable first accelerated path when OBS recycles its texture before the encoder finishes. Report the copy and its cost; do not call that direct surface reuse.

The pinned OBS 32.2.2 interfaces to investigate are `obs_encoder_info::encode_texture2`, `encoder_texture`, `gs_texture_get_obj`, and the platform encoder implementations. A native texture pointer alone does not transfer pool ownership, device affinity, or synchronization.

- [OBS encoder interface](https://github.com/obsproject/obs-studio/blob/32.2.2/libobs/obs-encoder.h)
- [OBS graphics interface](https://github.com/obsproject/obs-studio/blob/32.2.2/libobs/graphics/graphics.h)

## Quests

- [Encoder adapter](/quest/m2/obs-moq-video/adapter.md) - establish the OBS integration and owned frame/packet contract with a working CPU baseline
- [macOS GPU input](/quest/m2/obs-moq-video/macos.md) - feed VideoToolbox without CPU readback from the OBS compositor
- [Windows GPU input](/quest/m2/obs-moq-video/windows.md) - import or blit OBS D3D11 textures with explicit device and keyed-mutex ownership
- [Linux GPU input](/quest/m2/obs-moq-video/linux.md) - establish an exportable OBS allocation and import it into a supported hardware encoder

## Related

- [OBS callback lifetime](/quest/m0/obs-session-callback-lifetime.md) - shared session teardown must remain safe under a delayed terminal
- [Video hardware validation](/quest/m3/video-hardware.md) - validate native paths on actual hardware
