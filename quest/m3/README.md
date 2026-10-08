# m3: deferred

## Goal

Work that waits on something besides priority: either a first step outside
this repository (hardware nobody on the team has, a partner or customer who
asks for it, a hosting provider's offer, or an upstream release), or a
speculative feature or study with no named consumer yet.

## Plan

A quest gated on the outside world has a condition quest beside it, as the
root [questline](/quest/README.md) describes. A speculative feature or study
with no consumer needs no condition quest: it parks here rather than in m2
until a consumer appears, and is deleted when it goes stale; git history keeps
it. Decided in the 2026-10-05 audit: both kinds live here, and the m2 quests
deferred for having no named consumer moved here. The 2026-10-08 audit folded
m4's upstream waits in here and moved m2 work with no consumer.

## Required

- [Video validation hardware is on hand](/quest/m3/video-hardware-access.md) - the machines the validation runs on, including a Raspberry Pi 4/5 for embedded presenting
- [Video hardware validation](/quest/m3/video-hardware.md) - run the encode, capture, and zero-copy paths that were never run on real machines, including PipeWire on KDE, the camera portal, and a Pi
- [An Ada NVIDIA GPU is available](/quest/m3/ada-gpu.md) - the hardware AV1 NVENC is verified on
- [NVENC AV1](/quest/m3/nvenc-av1.md) - AV1 encode through NVENC, once an Ada-generation GPU is available
- [Catalog colour model](/quest/m3/color-catalog.md) - the catalog describes a rendition's colour and HDR properties once a renderer consumes them
- [#2147](/quest/m3/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - NVIDIA Main10 encode, once a 10-bit source exists in the pipeline
- [Embedded video](/quest/m3/video-embedded.md) - verify a Pi 4/5 presents through the existing Vulkan and CPU paths, adding EGL import only if that fails
- [Multi-plane PipeWire cameras](/quest/m3/pipewire-camera-planes.md) - I420 and NV12 cameras that deliver one memory block per plane capture, if the hardware pass finds one
- [Video codec coverage](/quest/m3/video-codec-coverage.md) - prioritize remaining native AV1 and portable decoder gaps once a consumer asks
- [A physical-NIC peer for the zero-copy sweep](/quest/m3/zero-copy-peer.md) - the rig the zero-copy sweep runs on
- [#3201: moq-uring: use SENDMSG_ZC for large UDP GSO trains](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - complete the prerequisite issue first
- [#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [Stream sessions on the ring](/quest/m3/uring-tcp/README.md) - WebSocket and HTTP on the io_uring workers, gated on the ablation; no fleet asks for ring TCP yet
- [#3202](/quest/m3/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: fixed-file slots for worker UDP sockets, if the ablation measures a win
- [#3129](/quest/m3/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - moq-uring writes the WebTransport stream header at open and settles the open contract, once a caller hits it
- [QUIC receive timestamps](/quest/m3/quic-gcc.md) - a spike measuring receive timestamps in ACKs on native and relay-to-relay egress; delay-based control only if it justifies one
- [L4S on the backbone](/quest/m3/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement, if the ECN measurement says marks survive
- [Careful resume on reconnect](/quest/m3/quic-careful-resume.md) - a Rust redial starts at the previous connection's rate
- [Cut-through](/quest/m3/cut-through/README.md) - a lossy-hop bench decides whether a relay should forward bytes behind a QUIC stream hole; the build is re-planned if it says go
- [Unreal prototype](/quest/m3/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [vcpkg registry](/quest/m3/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests, once a consumer asks
- [Conan remote](/quest/m3/cpp-conan.md) - a remote we own serves the same tarball to `conan install`
- [Linux OBS GPU input](/quest/m3/obs-linux-gpu.md) - publish OBS compositor frames without CPU readback on a validated Linux graphics/encoder combination
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - design operator boundaries and policy without adding incomparable costs
- [moq-transport cluster peers](/quest/m3/ietf-cluster-peers.md) - an extended cluster draft lets moq-transport relays peer again, once a moq-transport relay operator asks
- [The moq.pro mesh runs lite-07](/quest/m3/lite07-mesh.md) - the deployment that makes the exemption dead code
- [Drop the hidden cluster exemption](/quest/m3/hidden-exemption.md) - relays stop forcing hidden broadcasts on cluster peers once every peer opts in on the wire
- [Robot teleoperation primitive](/quest/m3/teleop-robot.md) - a `moq-robot` crate for video down and control up, once a robotics consumer appears
- [Operator arbitration](/quest/m3/teleop-arbitration.md) - one operator holds control at a time
- [MAVLink bridge](/quest/m3/teleop-mavlink.md) - a `moq-mavlink` gateway
  replacing the VPN plus two unmanaged UDP flows, with QGroundControl and
  friends unchanged
- [SITL proof and browser ground station](/quest/m3/teleop-proof.md) - ArduPilot
  SITL and a synthetic camera flown from a browser ground station, reproducible
  in five minutes
- [P2P](/quest/m3/p2p/README.md) - opted-in clients serve each other over data channels and iroh while the relay stays the rendezvous and the fallback, under application policy
- [Ladder](/quest/m3/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [fMP4 emsg](/quest/m3/emsg.md) - settles the shared framing and missing-data semantics before this section adopts them
- [ID3 catalog section](/quest/m3/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [FLV script tags](/quest/m3/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [MSFTS convergence](/quest/m3/msfts-convergence.md) - the demultiplexed TS lane converges on MSFTS where they differ, once an MSFTS implementer engages
- [#2279](/quest/m3/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues arrive immediately on an independent metadata track, optionally associated with a rendition
- [CEA-608/708](/quest/m3/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [Browser archive](/quest/m3/archive-browser.md) - the same contract for browser-published broadcasts
- [Paced replay](/quest/m3/archive-paced-replay.md) - a replay pushes its groups to live subscribers on one shared clock, so any live player plays it
- [Encoder feedback](/quest/m3/stats-encoder-feedback.md) - a Rust encoder
  reads its viewers' feedback and adapts its bitrate
- [Closure counters](/quest/m3/closure-counters.md) - a departed node's return never regresses the closure counters a consumer already saw
- [Interop runner withdraws before disconnecting](/quest/m3/interop-runner-approval.md) - condition: the maintainer approves posting and a released moq-tokio and @moq/net carry close() withdrawals
- [Safari ships the WebKit 319818 fix](/quest/m3/webkit-319818.md) - the Safari release the gate admits
- [Safari WebTransport](/quest/m3/safari-webtransport.md) - WebKit browsers return to WebTransport once WebKit 319818 ships fixed
