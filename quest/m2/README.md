# m2: later work

## Goal

Later work: deferred features, design studies, and experiments.

## Plan

Nothing here blocks a release. Promote a quest into
[m1](/quest/m1/README.md) when it joins the next wave, including planning
work whose decisions are worth settling now; deferral does not abandon a
feature. A study may end with a measured no-go. Work gated on hardware, a
partner, a consumer, or a provider, or with no named consumer yet, waits in [m3](/quest/m3/README.md); work
waiting on an upstream release waits in [m4](/quest/m4/README.md).

## Required

- [RTSP import](/quest/m2/rtsp-import.md) - `moq import rtsp` publishes an IP camera from its own network, through a `moq-rtsp` crate whose one-session ingest a caller can supervise itself
- [One port](/quest/m2/one-port/README.md) - a relay speaks QUIC and STUN on one UDP port and HTTP, RTMP, and RTMPS on one TCP port; `moq-rtc` serves WebRTC media from the same UDP port
- [Bitrate claim](/quest/m2/rate-claim.md) - tokens and auth grants cap a session's upload and download bitrate, refused where unenforced and always on HTTP
- [QUIC caps](/quest/m2/rate-quic.md) - paced MAX_DATA credit in and a capped pacer out hold a QUIC session to its token's bitrate
- [WebSocket caps](/quest/m2/rate-websocket.md) - paced reads and writes over bounded socket buffers hold WebSocket to the same caps
- [Publishers learn their cap](/quest/m2/rate-grant.md) - the AUTH grant carries the caps and publishers clamp their encoder to them
- [One port on the io_uring workers](/quest/m2/uring-demux.md) - `moq-uring`'s workers host the UDP demux, so a ring relay keeps STUN, WebRTC, and SRT on its QUIC port
- [Synced data playback](/quest/m2/watch-data-sync.md) - js/watch releases JSON and binary payloads on the media playhead, and a slow data track holds media back
- [Watch decode gate](/quest/m2/watch-decode-gate.md) - video lookahead stays encoded until it is near presentation, like audio
- [Stream sessions](/quest/m2/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [Hitless TS legs](/quest/m2/ts-hitless.md) - two `--sync` export legs emit packet-identical TS for ST 2022-7
- [DVB E-AC-3](/quest/m2/ts-eac3.md) - E-AC-3 private data is split per sync frame and buffer-modelled in TS export
- [T-STD controls](/quest/m2/tstd-controls.md) - the harness gains an MB-overflow control and an AAC broadcast reference
- [MP4 export](/quest/m2/mp4-export.md) - `moq export mp4 --output` records crash-safe fragments, then finishes a regular MP4 with moov at the end
- [fMP4 edit lists](/quest/m2/fmp4-edit-lists.md) - the fMP4 importer applies edit lists to frame timestamps
- [Linux decoded frames](/quest/m2/obs-decode-linux.md) - present supported native decoded surfaces with visible CPU fallback
- [Windows decoded frames](/quest/m2/obs-decode-windows.md) - present decoded D3D11 surfaces in OBS without CPU readback
- [macOS GPU input](/quest/m2/obs-macos.md) - feed the encoder from the OBS compositor without CPU readback
- [Windows GPU input](/quest/m2/obs-windows.md) - import or blit OBS D3D11 textures with explicit synchronization
- [Sans-IO IETF session](/quest/m2/rs2ts-sans-io-ietf.md) - the session shape it translates
- [IETF parameters](/quest/m2/rs2ts-ietf-params.md) - the IETF codec drops its `Param` trait on primitives, so it translates like lite
- [Generated IETF](/quest/m2/rs2ts-ietf.md) - @moq/net's moq-transport session is generated too
- [Per-stream deadlines](/quest/m2/quic-deadline.md) - hopeless retransmits
  become resets, and a tail loss probe fires early while there is still time
- [qmux on the QUIC stream state machine](/quest/m2/quic-qmux.md) - qmux is a
  first-class crate in the fork over the shared stream state machine
- [Align BBR loss handling with draft-06](/quest/m2/quic-bbr-loss-parity.md) - losses use their own sample and undo re-enters ProbeUp through Refill
- [Measure ECN on the backbone](/quest/m2/quic-ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [Per-stream ACK progress](/quest/m2/quic-ack-progress.md) - `moq-quic` reports
  how far a send stream has been acknowledged and when
- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - the
  backend-neutral hook on moq-net's own transport trait that awaits an
  acknowledged stream offset, implemented by the moq-tokio and moq-uring adapters
- [Starvation at frame granularity](/quest/m2/starvation-frames.md) - the
  acknowledged frontier moves at every frame boundary through `poll_acked`,
  with a delivery-delay histogram for jitter
- [#3200](/quest/m2/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [#3202](/quest/m2/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: use fixed-file slots for worker UDP sockets
- [Open contract](/quest/m2/uring-open-contract.md) - settle concurrent ownership and backpressure before implementation
- [#3129](/quest/m2/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - moq-uring: write the WebTransport stream header at open time, so finish() never owes one
- [Cache shard](/quest/m2/cache-shard.md) - stop hammering one process-global cache line from every worker
- [Send depth](/quest/m2/send-depth.md) - moq-net futures prove Send at the default recursion limit, so downstream crates see no nightly lint
- [Refusal reasons](/quest/m2/refusal-reasons.md) - refused-session metrics tell an expired token from an invalid one, and count gateway admissions
- [Bench coverage](/quest/m2/bench-coverage.md) - Criterion targets for moq-pattern matching first, then moq-mux containers, the hang catalog, and moq-auth
- [Signed priority](/quest/m2/signed-priority.md) - every API priority is an `i8` with 0 as the unset midpoint, and hang's built-ins sit above it
- [mTLS on tls://](/quest/m2/tls-listener-mtls.md) - a `tls://` listener can identify a cluster peer by its client certificate
- [One transport adapter](/quest/m2/transport-adapter-dedup.md) - the poll transport adapter exists once, and the 64 KiB cap in moq-net's default `poll_read_buf` (not in the adapter) has a test
- [Catalog track identity](/quest/m2/catalog-tracks.md) - a changed track configuration becomes a new track name or epoch, never a mutated definition
- [Catalog colour model](/quest/m2/color-catalog.md) - the catalog describes a rendition's colour and HDR properties once a renderer consumes them
- [Archive S3 wire proof](/quest/m2/archive-s3.md) - the archive proof also runs through the S3 client against an in-process S3-compatible server
- [Archive recovery listing](/quest/m2/archive-recovery-listing.md) - a resumed DVR lists what changed since its checkpoint, not every stored group
- [DASH rendition URLs](/quest/m2/dash-rendition-uri.md) - DASH init and segment URLs percent-encode the rendition name, so a name with a slash resolves to its own rendition
- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - Linux relay packages ship io_uring once the ring is on par with tokio
- [iOS capture](/quest/m2/mobile-capture-ios.md) - Rust captures the camera and screen on iOS
- [Android capture](/quest/m2/mobile-capture-android.md) - Rust captures through NDK/JNI on Android, reusing the existing codecs
- [Mobile completion](/quest/m2/mobile-completion.md) - verify the selected native/mobile path before closing #700
- [Opus implementation](/quest/m2/audio-opus-backend.md) - compare Opus codec quality, CPU, build cost, and the loss recovery each backend offers
- [Latency ledger](/quest/m2/latency-ledger.md) - a session reports where its end-to-end audio delay went, stage by stage
- [JS LOC duration marker](/quest/m2/js-loc-duration-marker.md) - `@moq/loc`'s producer ends each video group with the empty duration frame, as moq-mux does
- [Media Foundation decode](/quest/m2/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m2/audio-encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec decode](/quest/m2/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m2/audio-encode-mediacodec.md) - Android encodes AAC-LC
- [Video codec coverage](/quest/m2/video-codec-coverage.md) - prioritize remaining native AV1 and portable decoder gaps
- [#2147](/quest/m2/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - moq-video: 10-bit HEVC in the NVIDIA codec path; AV1 is m3
- [NVENC buffer pool](/quest/m2/nvenc-pool.md) - NVENC reuses input and output buffers instead of allocating per frame, if a benchmark shows it wins
- [NVENC held frames](/quest/m2/nvenc-held-frames.md) - moq-nvenc refuses or drives configurations whose frames the driver holds back
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [Intra-refresh GOPs](/quest/m2/intra-refresh/README.md) - video with periodic intra refresh publishes, imports, and tunes in cleanly with one group per sweep and a catalog `warmup`
- [Capture multi-plane PipeWire cameras](/quest/m2/pipewire-camera-planes.md) - I420 and NV12 cameras that deliver one memory block per plane
- [#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: validate PipeWire DMA-BUFs into the Vulkan renderer on hardware
- [vcpkg registry](/quest/m2/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests
- [Binary delta stats](/quest/m2/stats-delta.md) - an on-demand varint delta flavor of every stats track, if relay encode CPU still matters after the JSON fixes
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - moq-uring receives from the buffer ring and transmits into registered buffers with no copy, once a profile says where
- [BBR media study](/quest/m2/quic-bbr-natural-drain.md) - whether bounded drain credit avoids ProbeRTT deadline interference, and where our BBR differs from Google's
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [Viewer up-switch](/quest/m2/viewer-upswitch.md) - a viewer capped by its small rendition finds headroom through PROBE and moves up
- [L4S on the backbone](/quest/m2/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/m2/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive by deadline](/quest/m2/quic-keep-alive.md) - a PING only when the idle deadline nears, no fixed timer
- [Socket close](/quest/m2/noq-socket-close.md) - `moq-quic` releases an endpoint's socket on close, so moq-tokio drops its wrapper
- [GOP overhead](/quest/m2/gop-overhead.md) - price the I-frames a short GOP pays for, deciding whether a long GOP plus a keyframe request is worth designing
- [TS health stats](/quest/m2/ts-health-stats.md) - the TS counters ride the stats plumbing beside the media counters
- [Teleoperation](/quest/m2/teleop/README.md) - MoQ carries robot video down and control up on one session as a library capability
- [Media QA on other engines](/quest/m2/browser-media-qa-engines.md) - the media harness measures a Firefox or WebKit player over the fallback and names what each engine lacks
- [Windows.Graphics.Capture](/quest/m2/capture-wgc.md) - the WGC display and window backend verified on real Windows hardware
- [Windows capture parity](/quest/m2/capture-windows.md) - system audio and a settled app-capture policy
- [Linux capture parity](/quest/m2/capture-linux.md) - Wayland window/system-audio capture with explicit display-selection and app-capture limits
- [Egress profile](/quest/m2/quic-egress-profile.md) - measure relay send-path syscalls, pacing bursts, and allocations before optimizing any of them
- [Compressed tracks](/quest/m2/flate.md) - moq-ffi and every wrapper expose flate tracks through a `flate` namespace like `json`
- [moq-transport cluster peers](/quest/m2/ietf-cluster-peers.md) - an extended cluster draft lets moq-transport relays peer again, carrying the cluster route layer
- [MSFTS convergence](/quest/m2/msfts-convergence.md) - the demultiplexed TS lane converges on MSFTS where the two still differ: program tables and the ES payload unit
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - H.265 encode and decode, pre-generated bindings, and pooled resize surfaces, including the moq-dev/vaapi release that carries them
- [One external GPU image for every encoder](/quest/m2/gpu-surface.md) - a vendor-neutral `Surface::Vulkan` that `Kind::Auto` routes to whichever backend imports it on its device; NVENC imports internally
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - H.264 and H.265 from an external Vulkan image on RADV, hand-rolled on ash, proven on an RX 9070
- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - an explicit-sync DMA-BUF reaches Intel's encoder, proven on Arrow Lake
- [A release carries multi-vendor GPU input](/quest/m2/gpu-release.md) - `release` ships all three so a pinned consumer drops its vendor code
- [GPU capacity and health](/quest/m2/gpu-health.md) - moq-video reports each device's sessions, memory, utilization, and health on NVIDIA, AMD, and Intel alike
- [Malformed moq-transport input](/quest/m2/ietf-malformed-close.md) - malformed draft-18 and draft-21 control input closes the session with the draft's code, or PROTOCOL_VIOLATION where a code is a real burden and the fallback is recorded in `doc/concept/standard.md`, in moq-net and js/net
- [moq-transport request codes](/quest/m2/ietf-request-codes.md) - Range Filters (INVALID_FILTER), reserved namespaces, and RENDEZVOUS_TIMEOUT get the draft's answer, or a recorded fallback code, and the deliberate deviations are documented
- [Leftover worklet types](/quest/m2/worklet-leftovers.md) - `@moq/hang` drops its unused `@types/audioworklet` dependency, and moq-boy stops including the shared worklet declaration
- [Native enabled](/quest/m2/native-enabled.md) - native players and the ffi/C paths never select a disabled rendition
- [JS audio ranking](/quest/m2/js-audio-ranked.md) - @moq/hang and HLS rank audio renditions like Rust
- [ts::Export catalog stream](/quest/m2/ts-export-catalog.md) - TS export takes (source, catalog) like the other exporters
- [C backend copy](/quest/m2/c-backend-copy.md) - one payload copy out of the C backend, benchmarked
- [Load-balancer refusals](/quest/m2/listener-lb-refusals.md) - refuse ignored or conflicting QUIC load-balancer settings
