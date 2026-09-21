# future

## Goal

Later work: deferred features, design studies, experiments, and quests gated
on the outside world (hardware nobody has, a partner, an upstream release).

## Plan

Nothing here blocks a release or has a branch. Promote a quest into
[next](/quest/next/README.md) when it joins the next wave, including planning
work whose decisions are worth settling now; deferral does not abandon a
feature. A study may end with a measured no-go. A quest gated on the outside
world states the condition as a plain-text `Required` bullet and waits here.

## Quests

- [AV1 metadata separation](/quest/future/av1-metadata.md) - retain metadata OBUs inline while evaluating separate delivery
- [SEI separation](/quest/future/sei/README.md) - retain inline SEI until measured savings or a metadata-only consumer justify a split
- [Catalog track identity](/quest/future/catalog-tracks.md) - compare immutable track definitions with explicit catalog-to-group binding
- [Mobile ownership](/quest/future/mobile-ownership.md) - decide whether Rust or platform code owns mobile capture, codecs, and rendering
- [iOS capture](/quest/future/mobile-capture-ios.md) - camera and screen capture if the mobile ownership decision selects Rust
- [Android capture](/quest/future/mobile-capture-android.md) - NDK/JNI capture using the existing codecs if mobile ownership selects Rust
- [Mobile completion](/quest/future/mobile-completion.md) - verify the selected native/mobile path before closing #700
- [Linux OBS GPU input](/quest/future/obs-linux-gpu.md) - publish OBS compositor frames without CPU readback on a validated Linux graphics/encoder combination
- [LiveKit client shim](/quest/future/livekit-shim.md) - a media compatibility facade over the room SDK
- [Audio loss recovery](/quest/future/audio-loss-recovery.md) - prove a useful Opus recovery policy before exposing another option
- [Opus implementation](/quest/future/audio-opus-backend.md) - compare current codec quality, CPU, and optional build costs
- [Video codec coverage](/quest/future/video-codec-coverage.md) - prioritize remaining native AV1 and portable decoder gaps
- [#2819](/quest/future/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: carry PipeWire DMA-BUFs safely into the Vulkan renderer
- [Unreal prototype](/quest/future/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [Unity prototype](/quest/future/unity.md) - the C# package under IL2CPP, playing subscribed audio
- [Multipath spike](/quest/future/multipath-spike.md) - whether bonded contribution over multipath QUIC is worth building, given it needs noq on both ends
- [Receive timestamps](/quest/future/quic-receive-ts.md) - per-packet arrival times in ACKs, the feedback GCC and deadlines need
- [QUIC GCC](/quest/future/quic-gcc.md) - a measured verdict on delay-based congestion control for media egress, shipping as `RealTime`
- [QUIC FEC](/quest/future/quic-fec.md) - a measured verdict on transport-level FEC vs retransmission
- [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) - whether ProbeRTT and the bandwidth model behave for a sender at the encoder's rate
- [Kernel pacing](/quest/future/quic-kernel-pacing.md) - whether SO_TXTIME pacing beats a userspace pacer the io_uring driver ignores today
- [Send batching](/quest/future/quic-send-batching.md) - whether sendmmsg across connections pays on the tokio path
- [Send buffer pools](/quest/future/quic-buffer-pool.md) - whether pooled send buffers beat Bytes in the stream send path
- [AF_XDP UDP path](/quest/future/af-xdp.md) - the kernel-bypass verdict on today's virtio hosts that gates DPDK
- [GOP overhead](/quest/future/gop-overhead.md) - price the I-frames a short GOP pays for, deciding whether a long GOP plus a keyframe request is worth designing
- [#703](/quest/future/703-experimental-webgpu-renderer.md) - Experimental WebGPU renderer
- [#823](/quest/future/823-svc-support.md) - SVC support?
- [#1838](/quest/future/1838-tr-101-290-monitoring-requirements-broadcast-contribution.md) - TR 101 290 monitoring: requirements (broadcast/contribution health metrics)
- [Teleoperation](/quest/future/teleop/README.md) - MoQ carries robot video down and control up on one session as a library capability
- [SIP media stack](/quest/future/sip-stack.md) - terminate one inbound SIP audio call leg and expose it as Opus frames
- [Carrier voice](/quest/future/carrier-voice/README.md) - determine whether MoQ should be the call fabric for programmable carrier voice
- [LiveKit WebRTC bridge](/quest/future/livekit-webrtc-bridge.md) - a go/no-go verdict, backed by a spike, on per-track LiveKit-to-MoQ bridging
- [Common Access Tokens](/quest/future/cat/README.md) - a moq-transport client presents a CAT in SETUP and `moq auth serve` admits it with the scope its `moqt` claim names
- [DPDK](/quest/future/dpdk.md) - a kernel-bypass UDP path for the relay, once a provider offers SR-IOV or bare metal
- [Video hardware validation](/quest/future/video-hardware.md) - run the encode, capture, and zero-copy paths that were written but never run on real machines
- [#2893](/quest/future/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - video: validate PipeWire DMA-BUF capture on KDE hardware
- [Embedded video](/quest/future/video-embedded.md) - EGL import in the renderer, so moq-video presents on a Pi
- [Vision worker](/quest/future/processor-vision.md) - a documented customer-run vision worker proves the processor contract
