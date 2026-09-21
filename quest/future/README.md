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

- [Go vanity docs](/quest/future/go-vanity-docs.md) - the Go page names only `moq.dev/moq` once the republished mirrors declare it
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
- [Latency ledger](/quest/future/latency-ledger.md) - a session reports where its end-to-end audio delay went, stage by stage
- [Media Foundation decode](/quest/future/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/future/audio-encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec decode](/quest/future/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/future/audio-encode-mediacodec.md) - Android encodes AAC-LC
- [Video codec coverage](/quest/future/video-codec-coverage.md) - prioritize remaining native AV1 and portable decoder gaps
- [#2147](/quest/future/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - moq-video: 10-bit HEVC and AV1 support in the NVIDIA codec path
- [VAAPI encode and decode](/quest/future/video-vaapi.md) - DMA-BUF encode, H.265 decode, and pre-generated bindings that remove the libclang build dependency, all gated on a moq-dev/vaapi release
- [Direct3D11 render import](/quest/future/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [Intra-refresh GOPs](/quest/future/intra-refresh/README.md) - video with periodic intra refresh publishes, imports, and tunes in cleanly with one group per sweep and a catalog `warmup`
- [#2819](/quest/future/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: carry PipeWire DMA-BUFs safely into the Vulkan renderer
- [Unreal prototype](/quest/future/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [Unity prototype](/quest/future/unity.md) - the C# package under IL2CPP, playing subscribed audio
- [#2907](/quest/future/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - the browser reaches moq-ffi through a generated TypeScript binding once a JS generator is stable
- [C# through moq-ffi](/quest/future/cs/README.md) - generated C# over moq-ffi as a NuGet package with native runtimes
- [vcpkg registry](/quest/future/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests
- [Conan remote](/quest/future/cpp-conan.md) - a remote we own serves the same tarball to `conan install`
- [Compressed tracks](/quest/future/flate/README.md) - any track compresses per group from every language, not only the JSON modes
- [#3115](/quest/future/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [Safari WebTransport](/quest/future/safari-webtransport.md) - WebKit browsers return to WebTransport once WebKit 319818 ships fixed
- [Multipath spike](/quest/future/multipath-spike.md) - whether bonded contribution over multipath QUIC is worth building, given it needs noq on both ends
- [Receive timestamps](/quest/future/quic-receive-ts.md) - per-packet arrival times in ACKs, the feedback GCC and deadlines need
- [QUIC GCC](/quest/future/quic-gcc.md) - a measured verdict on delay-based congestion control for media egress, shipping as `RealTime`
- [QUIC FEC](/quest/future/quic-fec.md) - a measured verdict on transport-level FEC vs retransmission
- [Google BBR comparison](/quest/future/quic-bbr-google.md) - measure growth detection and precautionary probing after the correctness fixes
- [Natural media drains](/quest/future/quic-bbr-app-limited.md) - whether bounded drain credit avoids ProbeRTT deadline interference
- [Discover media headroom](/quest/future/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [L4S on the backbone](/quest/future/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/future/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive by deadline](/quest/future/quic-keep-alive.md) - a PING only when the idle deadline nears, no fixed timer
- [Routing cost domains](/quest/future/routing-cost-domains.md) - design operator boundaries and policy without adding incomparable costs
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
- [Runtime QA hosts](/quest/future/runtime-qa-hosts.md) - run exact source snapshots on accessible Linux and device hosts with retrievable debug evidence
- [Media QA on other engines](/quest/future/browser-media-qa-engines.md) - the media harness measures a Firefox or WebKit player over the fallback and names what each engine lacks
- [Windows capture parity](/quest/future/capture-windows.md) - system audio and screen cursor capture with a settled app-capture policy
- [Linux capture parity](/quest/future/capture-linux.md) - Wayland window/system-audio capture with explicit display-selection and app-capture limits
- [Plan capture ergonomics](/quest/future/capture-ergonomics.md) - scope independent crop and audio mixing quests
- [X11 capture transport](/quest/future/x11-capture-shm.md) - move X11 capture to shared memory and RandR events instead of a per-frame socket copy
- [Capture frame buffers](/quest/future/capture-frame-buffers.md) - stop rebuilding a full-frame buffer every tick in the X11 and Windows backends
- [#2893](/quest/future/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - video: validate PipeWire DMA-BUF capture on KDE hardware
- [Embedded video](/quest/future/video-embedded.md) - EGL import in the renderer, so moq-video presents on a Pi
- [Vision worker](/quest/future/processor-vision.md) - a documented customer-run vision worker proves the processor contract
