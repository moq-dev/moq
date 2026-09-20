# m3: later work

## Goal

Deferred features, design studies, and experiments; hardware- and
partner-gated work sits in m4. Exploratory quests may end with a measured
no-go verdict; implementation quests retain their stated completion criteria.

## Plan

Nothing in this milestone blocks a release. Promote work into m2 when it
joins the next agent wave, including planning work when its decisions are
worth settling now. Deferral does not by itself abandon a feature.

## Quests

- [AV1 metadata separation](/quest/m3/av1-metadata.md) - retain metadata OBUs inline while evaluating separate delivery

- [SEI separation](/quest/m3/sei/README.md) - retain inline SEI until measured savings or a metadata-only consumer justify a split

- [Catalog track identity](/quest/m3/catalog-tracks.md) - compare immutable track definitions with explicit catalog-to-group binding
- [Mobile ownership](/quest/m3/mobile-ownership.md) - decide whether Rust or platform code owns mobile capture, codecs, and rendering
- [iOS capture](/quest/m3/mobile-capture-ios.md) - camera and screen capture if the mobile ownership decision selects Rust
- [Android capture](/quest/m3/mobile-capture-android.md) - NDK/JNI capture using the existing codecs if mobile ownership selects Rust
- [Mobile completion](/quest/m3/mobile-completion.md) - verify the selected native/mobile path before closing #700
- [Linux OBS GPU input](/quest/m3/obs-linux-gpu.md) - publish OBS compositor frames without CPU readback on a validated Linux graphics/encoder combination
- [LiveKit client shim](/quest/m3/livekit-shim.md) - a media compatibility facade over the room SDK

- [Audio loss recovery](/quest/m3/audio-loss-recovery.md) - prove a useful Opus recovery policy before exposing another option
- [Opus implementation](/quest/m3/audio-opus-backend.md) - compare current codec quality, CPU, and optional build costs
- [Video codec coverage](/quest/m3/video-codec-coverage.md) - prioritize remaining native AV1 and portable decoder gaps
- [#2819](/quest/m3/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: carry PipeWire DMA-BUFs safely into the Vulkan renderer
- [Unreal prototype](/quest/m3/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [Unity prototype](/quest/m3/unity.md) - the C# package under IL2CPP, playing subscribed audio
- [Multipath spike](/quest/m3/multipath-spike.md) - whether bonded contribution over multipath QUIC is worth building, given it needs noq on both ends
- [Receive timestamps](/quest/m3/quic-receive-ts.md) - per-packet arrival times in ACKs, the feedback GCC and deadlines need
- [QUIC GCC](/quest/m3/quic-gcc.md) - a measured verdict on delay-based congestion control for media egress, shipping as `RealTime`
- [QUIC FEC](/quest/m3/quic-fec.md) - a measured verdict on transport-level FEC vs retransmission
- [BBR3 app-limited](/quest/m3/quic-bbr-app-limited.md) - whether ProbeRTT and the bandwidth model behave for a sender at the encoder's rate
- [Kernel pacing](/quest/m3/quic-kernel-pacing.md) - whether SO_TXTIME pacing beats a userspace pacer the io_uring driver ignores today
- [Send batching](/quest/m3/quic-send-batching.md) - whether sendmmsg across connections pays on the tokio path
- [Send buffer pools](/quest/m3/quic-buffer-pool.md) - whether pooled send buffers beat Bytes in the stream send path
- [AF_XDP UDP path](/quest/m3/af-xdp.md) - the kernel-bypass verdict on today's virtio hosts that gates DPDK
- [GOP overhead](/quest/m3/gop-overhead.md) - price the I-frames a short GOP pays for, deciding whether a long GOP plus a keyframe request is worth designing
- [#703](/quest/m3/703-experimental-webgpu-renderer.md) - Experimental WebGPU renderer
- [#823](/quest/m3/823-svc-support.md) - SVC support?
- [#1838](/quest/m3/1838-tr-101-290-monitoring-requirements-broadcast-contribution.md) - TR 101 290 monitoring: requirements (broadcast/contribution health metrics)
- [Teleoperation](/quest/m3/teleop/README.md) - MoQ carries robot video down and control up on one session as a library capability
- [SIP media stack](/quest/m3/sip-stack.md) - terminate one inbound SIP audio call leg and expose it as Opus frames
- [Carrier voice](/quest/m3/carrier-voice/README.md) - determine whether MoQ should be the call fabric for programmable carrier voice
- [LiveKit WebRTC bridge](/quest/m3/livekit-webrtc-bridge.md) - a go/no-go verdict, backed by a spike, on per-track LiveKit-to-MoQ bridging
- [Common Access Tokens](/quest/m3/cat/README.md) - a moq-transport client presents a CAT in SETUP and `moq auth serve` admits it with the scope its `moqt` claim names
