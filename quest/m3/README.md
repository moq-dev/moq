# m3: deferred

## Goal

Work whose first step is outside this repository: hardware nobody on the team
has, a partner or customer, or a hosting provider's offer. Nothing here can
start by opening an editor.

## Plan

A quest lands here when its gate is the outside world, not its priority. Each
states the condition in prose or as a plain-text `Required` bullet. When the
condition clears, move the quest to the milestone its work belongs in.

## Required

- [Video hardware validation](/quest/m3/video-hardware.md) - run the encode, capture, and zero-copy paths that were written but never run on real machines
- [Embedded video](/quest/m3/video-embedded.md) - EGL import in the renderer, so moq-video presents on a Pi
- [#3201: moq-uring: use SENDMSG_ZC for large UDP GSO trains](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - complete the prerequisite issue first
- [#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [Receive timestamps](/quest/m3/quic-receive-ts.md) - per-packet arrival times in ACKs, the feedback GCC and deadlines need
- [QUIC GCC](/quest/m3/quic-gcc.md) - a measured verdict on delay-based congestion control for media egress, shipping as `RealTime`
- [AF_XDP UDP path](/quest/m3/af-xdp.md) - the kernel-bypass verdict on today's virtio hosts that gates DPDK
- [C# through moq-ffi](/quest/m3/cs/README.md) - generated C# over moq-ffi as a NuGet package with native runtimes
- [Unity prototype](/quest/m3/unity.md) - the C# package under IL2CPP, playing subscribed audio
- [Unreal prototype](/quest/m3/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [LiveKit client shim](/quest/m3/livekit-shim.md) - a media compatibility facade over the room SDK
- [Conan remote](/quest/m3/cpp-conan.md) - a remote we own serves the same tarball to `conan install`
- [WHEP ABR](/quest/m3/whep-abr.md) - a WHEP viewer switches renditions from its own congestion feedback
- [Synced data playback](/quest/m3/watch-data-sync.md) - js/watch releases JSON and binary payloads on the media playhead, and a slow data track holds media back
- [Linux OBS GPU input](/quest/m3/obs-linux-gpu.md) - publish OBS compositor frames without CPU readback on a validated Linux graphics/encoder combination
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - design operator boundaries and policy without adding incomparable costs
- [Common Access Tokens](/quest/m3/cat/README.md) - a moq-transport client presents a CAT in SETUP and `moq auth serve` admits it with the scope its `moqt` claim names
- [Drop the hidden cluster exemption](/quest/m3/hidden-exemption.md) - relays stop forcing hidden broadcasts on cluster peers once every peer opts in on the wire
- [MAVLink bridge](/quest/m3/teleop-mavlink.md) - a `moq-mavlink` gateway
  replacing the VPN plus two unmanaged UDP flows, with QGroundControl and
  friends unchanged
- [SITL proof and browser ground station](/quest/m3/teleop-proof.md) - ArduPilot
  SITL and a synthetic camera flown from a browser ground station, reproducible
  in five minutes
- [Browser teleoperation package](/quest/m3/teleop-browser-package.md) - `@moq/robot`
  mirrors the Rust crate, so browser clients consume the catalog and delivery
  classes
- [Encode config](/quest/m3/intra-refresh-encode-config.md) - refresh mode extends the settled GOP contract; the producer cuts groups per sweep and publishes `warmup`
- [NVENC refresh](/quest/m3/intra-refresh-nvenc.md) - the NVENC backend encodes refresh mode for H.264 and HEVC
- [V4L2 refresh](/quest/m3/intra-refresh-v4l2.md) - the V4L2 backend encodes refresh mode
- [Bindings](/quest/m3/intra-refresh-bindings.md) - moq-ffi and every wrapper expose refresh mode, additive on the ffi-shape `Gop` enum
