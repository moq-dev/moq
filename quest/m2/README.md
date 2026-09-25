# m2: later work

## Goal

Later work: deferred features, design studies, and experiments.

## Plan

Nothing here blocks a release. Promote a quest into
[m1](/quest/m1/README.md) when it joins the next wave, including planning
work whose decisions are worth settling now; deferral does not abandon a
feature. A study may end with a measured no-go. Work gated on hardware, a
partner, or a provider waits in [m3](/quest/m3/README.md); work waiting on an
upstream release waits in [m4](/quest/m4/README.md).

## Quests

- [AV1 metadata separation](/quest/m2/av1-metadata.md) - retain metadata OBUs inline while evaluating separate delivery
- [SEI separation](/quest/m2/sei/README.md) - retain inline SEI until measured savings or a metadata-only consumer justify a split
- [Catalog track identity](/quest/m2/catalog-tracks.md) - compare immutable track definitions with explicit catalog-to-group binding
- [Mobile ownership](/quest/m2/mobile-ownership.md) - decide whether Rust or platform code owns mobile capture, codecs, and rendering
- [iOS capture](/quest/m2/mobile-capture-ios.md) - camera and screen capture if the mobile ownership decision selects Rust
- [Android capture](/quest/m2/mobile-capture-android.md) - NDK/JNI capture using the existing codecs if mobile ownership selects Rust
- [Mobile completion](/quest/m2/mobile-completion.md) - verify the selected native/mobile path before closing #700
- [Linux OBS GPU input](/quest/m2/obs-linux-gpu.md) - publish OBS compositor frames without CPU readback on a validated Linux graphics/encoder combination
- [LiveKit client shim](/quest/m2/livekit-shim.md) - a media compatibility facade over the room SDK
- [Audio loss recovery](/quest/m2/audio-loss-recovery.md) - prove a useful Opus recovery policy before exposing another option
- [Opus implementation](/quest/m2/audio-opus-backend.md) - compare current codec quality, CPU, and optional build costs
- [Latency ledger](/quest/m2/latency-ledger.md) - a session reports where its end-to-end audio delay went, stage by stage
- [JS discontinuity](/quest/m2/js-discontinuity.md) - JS names its timeline break `discontinuity()` like Rust, so `cut` means the same group close in both
- [Synced data playback](/quest/m2/watch-data-sync.md) - js/watch releases JSON and binary payloads on the media playhead, and a slow data track holds media back
- [Media Foundation decode](/quest/m2/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m2/audio-encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec decode](/quest/m2/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m2/audio-encode-mediacodec.md) - Android encodes AAC-LC
- [Video codec coverage](/quest/m2/video-codec-coverage.md) - prioritize remaining native AV1 and portable decoder gaps
- [#2147](/quest/m2/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - moq-video: 10-bit HEVC and AV1 support in the NVIDIA codec path
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - video with periodic intra refresh publishes, imports, and tunes in cleanly with one group per sweep and a catalog `warmup`
- [#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: carry PipeWire DMA-BUFs safely into the Vulkan renderer
- [Unreal prototype](/quest/m2/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [Unity prototype](/quest/m2/unity.md) - the C# package under IL2CPP, playing subscribed audio
- [C# through moq-ffi](/quest/m2/cs/README.md) - generated C# over moq-ffi as a NuGet package with native runtimes
- [vcpkg registry](/quest/m2/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests
- [Conan remote](/quest/m2/cpp-conan.md) - a remote we own serves the same tarball to `conan install`
- [Compressed tracks](/quest/m2/flate/README.md) - any track compresses per group from every language, not only the JSON modes
- [Legacy stats names](/quest/m2/stats-legacy-names.md) - stats serializers write only the `*_started` / `*_ended` counter names once moq.pro reads them
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [Redundant ingest](/quest/m2/redundant-ingest.md) - decide whether two publishers sharing one epoch may splice, and who declares the incumbent dead before the keep-alive does
- [Multipath spike](/quest/m2/multipath-spike.md) - whether bonded contribution over multipath QUIC is worth building, given it needs noq on both ends
- [Receive timestamps](/quest/m2/quic-receive-ts.md) - per-packet arrival times in ACKs, the feedback GCC and deadlines need
- [QUIC GCC](/quest/m2/quic-gcc.md) - a measured verdict on delay-based congestion control for media egress, shipping as `RealTime`
- [QUIC FEC](/quest/m2/quic-fec.md) - a measured verdict on transport-level FEC vs retransmission
- [Google BBR comparison](/quest/m2/quic-bbr-google.md) - measure growth detection and precautionary probing after the correctness fixes
- [Natural media drains](/quest/m2/quic-bbr-app-limited.md) - whether bounded drain credit avoids ProbeRTT deadline interference
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [L4S on the backbone](/quest/m2/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/m2/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive by deadline](/quest/m2/quic-keep-alive.md) - a PING only when the idle deadline nears, no fixed timer
- [Bounded announce prefix table](/quest/m2/announce-prefix-table.md) - compress repeated path tuples on each ordered lite-07 announce stream, with bounded state and measured QUIC-byte savings
- [Routing cost domains](/quest/m2/routing-cost-domains.md) - design operator boundaries and policy without adding incomparable costs
- [Kernel pacing](/quest/m2/quic-kernel-pacing.md) - whether SO_TXTIME pacing beats a userspace pacer the io_uring driver ignores today
- [Send batching](/quest/m2/quic-send-batching.md) - whether sendmmsg across connections pays on the tokio path
- [Send buffer pools](/quest/m2/quic-buffer-pool.md) - whether pooled send buffers beat Bytes in the stream send path
- [AF_XDP UDP path](/quest/m2/af-xdp.md) - the kernel-bypass verdict on today's virtio hosts that gates DPDK
- [GOP overhead](/quest/m2/gop-overhead.md) - price the I-frames a short GOP pays for, deciding whether a long GOP plus a keyframe request is worth designing
- [#703](/quest/m2/703-experimental-webgpu-renderer.md) - Experimental WebGPU renderer
- [#823](/quest/m2/823-svc-support.md) - SVC support?
- [#1838](/quest/m2/1838-tr-101-290-monitoring-requirements-broadcast-contribution.md) - TR 101 290 monitoring: requirements (broadcast/contribution health metrics)
- [Teleoperation](/quest/m2/teleop/README.md) - MoQ carries robot video down and control up on one session as a library capability
- [SIP media stack](/quest/m2/sip-stack.md) - terminate one inbound SIP audio call leg and expose it as Opus frames
- [Carrier voice](/quest/m2/carrier-voice/README.md) - determine whether MoQ should be the call fabric for programmable carrier voice
- [LiveKit WebRTC bridge](/quest/m2/livekit-webrtc-bridge.md) - a go/no-go verdict, backed by a spike, on per-track LiveKit-to-MoQ bridging
- [Common Access Tokens](/quest/m2/cat/README.md) - a moq-transport client presents a CAT in SETUP and `moq auth serve` admits it with the scope its `moqt` claim names
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - run exact source snapshots on accessible Linux and device hosts with retrievable debug evidence
- [Media QA on other engines](/quest/m2/browser-media-qa-engines.md) - the media harness measures a Firefox or WebKit player over the fallback and names what each engine lacks
- [Windows capture parity](/quest/m2/capture-windows.md) - system audio and screen cursor capture with a settled app-capture policy
- [Linux capture parity](/quest/m2/capture-linux.md) - Wayland window/system-audio capture with explicit display-selection and app-capture limits
- [Plan capture ergonomics](/quest/m2/capture-ergonomics.md) - scope independent crop and audio mixing quests
- [X11 capture transport](/quest/m2/x11-capture-shm.md) - move X11 capture to shared memory and RandR events instead of a per-frame socket copy
- [Capture frame buffers](/quest/m2/capture-frame-buffers.md) - stop rebuilding a full-frame buffer every tick in the X11 and Windows backends
