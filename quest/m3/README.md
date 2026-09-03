# m3: prototypes

## Goal

Experiments, spikes, hardware validation, and measured go/no-go verdicts.
A written-down abandonment is a valid outcome for everything here.

## Plan

Nothing in this milestone blocks a release. Promote a quest into m2 when
its verdict lands and the follow-on work becomes concrete.

## Quests

- [Embedded video](/quest/m3/video-embedded.md) - V4L2 M2M codecs and EGL import, so moq-video works on a Pi
- [#2819](/quest/m3/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: carry PipeWire DMA-BUFs safely into the Vulkan renderer
- [#2893](/quest/m3/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - video: validate PipeWire DMA-BUF capture on KDE hardware
- [Video hardware validation](/quest/m3/video-hardware.md) - run the encode, capture, and zero-copy paths that were written but never run on real machines
- [Multipath spike](/quest/m3/multipath-spike.md) - whether bonded contribution over multipath QUIC is worth building, given it needs noq on both ends
- [QUIC GCC](/quest/m3/quic-gcc.md) - a measured verdict on delay-based congestion control for media egress
- [QUIC FEC](/quest/m3/quic-fec.md) - a measured verdict on transport-level FEC vs retransmission
- [SEI delivery](/quest/m3/sei-delivery.md) - prove a sidecar can reach a live stitcher before video; a no-go keeps SEI in-band
- [GOP overhead](/quest/m3/gop-overhead.md) - price the I-frames a short GOP pays for, deciding whether a long GOP plus a keyframe request is worth designing
- [#697](/quest/m3/697-conferencing-demo.md) - Conferencing Demo
- [#703](/quest/m3/703-experimental-webgpu-renderer.md) - Experimental WebGPU renderer
- [#823](/quest/m3/823-svc-support.md) - SVC support?
- [#1838](/quest/m3/1838-tr-101-290-monitoring-requirements-broadcast-contribution.md) - TR 101 290 monitoring: requirements (broadcast/contribution health metrics)
- [Teleoperation](/quest/m3/teleop/README.md) - MoQ carries robot video down and control up on one session as a library capability
- [SIP media stack](/quest/m3/sip-stack.md) - terminate one inbound SIP audio call leg and expose it as Opus frames
- [Carrier voice](/quest/m3/carrier-voice/README.md) - determine whether MoQ should be the call fabric for programmable carrier voice
- [LiveKit WebRTC bridge](/quest/m3/livekit-webrtc-bridge.md) - a go/no-go verdict, backed by a spike, on per-track LiveKit-to-MoQ bridging
- [Vision worker](/quest/m3/processor-vision.md) - a documented customer-run vision worker proves the processor contract
