# m3: deferred

## Goal

Work that waits on something besides priority: either a first step outside
this repository (hardware nobody on the team has, a partner or customer who
asks for it, or a hosting provider's offer), or a speculative feature or study
with no named consumer yet.

## Plan

A quest gated on the outside world has a condition quest beside it, as the
root [questline](/quest/README.md) describes. A speculative feature or study
with no consumer needs no condition quest: it parks here rather than in m2
until a consumer appears, and is deleted when it goes stale; git history keeps
it. Decided in the 2026-10-05 audit: both kinds live here, and the m2 quests
deferred for having no named consumer moved here.

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
- [Cut-through](/quest/m3/cut-through/README.md) - a relay forwards bytes behind a QUIC stream hole before the retransmission fills it, if a lossy-hop bench says it is worth it
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
- [Browser teleoperation package](/quest/m3/teleop-browser-package.md) - `@moq/robot`
  mirrors the Rust crate, so browser clients consume the catalog and delivery
  classes
- [SITL proof and browser ground station](/quest/m3/teleop-proof.md) - ArduPilot
  SITL and a synthetic camera flown from a browser ground station, reproducible
  in five minutes
- [Encode config](/quest/m3/intra-refresh-encode-config.md) - refresh mode extends the settled GOP contract; the producer cuts groups per sweep and publishes `warmup`
- [NVENC refresh](/quest/m3/intra-refresh-nvenc.md) - the NVENC backend encodes refresh mode for H.264 and HEVC
- [V4L2 refresh](/quest/m3/intra-refresh-v4l2.md) - the V4L2 backend encodes refresh mode
- [Bindings](/quest/m3/intra-refresh-bindings.md) - moq-ffi and every wrapper expose refresh mode, additive on the ffi-shape `Gop` enum
- [P2P](/quest/m3/p2p/README.md) - opted-in clients serve each other over data channels and iroh while the relay stays the rendezvous and the fallback, under application policy
- [Route trust](/quest/m3/route-trust.md) - a peer grant lets a client link advertise the nodes behind it as one identity; P2P is its first consumer
- [Ladder](/quest/m3/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [Processor](/quest/m3/processor/README.md) - a customer-run worker publishes an on-demand contribution under its own service prefix with scoped access
- [fMP4 emsg](/quest/m3/emsg.md) - settles the shared framing and missing-data semantics before this section adopts them
- [ID3 catalog section](/quest/m3/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [FLV script tags](/quest/m3/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [#2279](/quest/m3/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues arrive immediately on an independent metadata track, optionally associated with a rendition
- [CEA-608/708](/quest/m3/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [Browser archive](/quest/m3/archive-browser.md) - the same contract for browser-published broadcasts
- [Paced replay](/quest/m3/archive-paced-replay.md) - a replay pushes its groups to live subscribers on one shared clock, so any live player plays it
- [`moq relay`](/quest/m3/moq-relay-subcommand.md) - the relay runs under a `moq` verb with its own flags and TOML, while `moq-relay` stays a minimal binary
- [Encoder feedback](/quest/m3/stats-encoder-feedback.md) - a Rust encoder
  reads its viewers' feedback and adapts its bitrate
- [Text availability](/quest/m3/text-schema.md) - a text track publishes its own coverage index instead of copying the media timeline
- [Closure counters](/quest/m3/closure-counters.md) - a departed node's return never regresses the closure counters a consumer already saw
- [The maintainer approves posting to moq-interop-runner](/quest/m3/interop-runner-approval.md) - condition: the runner change may be posted
- [Interop graceful close](/quest/m3/interop-graceful-close.md) - successful runner publications withdraw before disconnecting
