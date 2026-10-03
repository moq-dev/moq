# m3: deferred

## Goal

Work whose first step is outside this repository: hardware nobody on the team
has, a partner or customer who asks for it, or a hosting provider's offer.
Nothing here should start by opening an editor.

## Plan

A quest lands here when its gate is the outside world, not its priority. Its gate
is a condition quest beside it, as the root [questline](/quest/README.md)
describes. A
speculative feature with no consumer parks here rather than in m2, and is
deleted when it goes stale; git history keeps it.

## Required

- [Video validation hardware is on hand](/quest/m3/video-hardware-access.md) - the machines the validation runs on
- [Video hardware validation](/quest/m3/video-hardware.md) - run the encode, capture, and zero-copy paths that were never run on real machines, including PipeWire on KDE, the camera portal, and a Pi
- [An Ada NVIDIA GPU is available](/quest/m3/ada-gpu.md) - the hardware AV1 NVENC is verified on
- [NVENC AV1](/quest/m3/nvenc-av1.md) - AV1 encode through NVENC, once an Ada-generation GPU is available
- [An embedded video device is on hand](/quest/m3/embedded-device.md) - the device EGL import is validated on
- [Embedded video](/quest/m3/video-embedded.md) - EGL import in the renderer, so moq-video presents on a Pi
- [A physical-NIC peer for the zero-copy sweep](/quest/m3/zero-copy-peer.md) - the rig the zero-copy sweep runs on
- [#3201: moq-uring: use SENDMSG_ZC for large UDP GSO trains](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - complete the prerequisite issue first
- [#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [QUIC GCC](/quest/m3/quic-gcc.md) - receive timestamps in ACKs and a measured verdict on delay-based congestion control for media egress, shipping as `RealTime`
- [AF_XDP UDP path](/quest/m3/af-xdp.md) - the kernel-bypass verdict on today's virtio hosts that gates DPDK
- [Unreal prototype](/quest/m3/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [Conan remote](/quest/m3/cpp-conan.md) - a remote we own serves the same tarball to `conan install`
- [Linux OBS GPU input](/quest/m3/obs-linux-gpu.md) - publish OBS compositor frames without CPU readback on a validated Linux graphics/encoder combination
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - design operator boundaries and policy without adding incomparable costs
- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - the deployment that makes the exemption dead code
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
