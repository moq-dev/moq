# m2: later work

## Goal

Later work: deferred features, design studies, and experiments.

## Plan

Nothing here blocks a release. Promote a quest into
[m1](/quest/m1/README.md) when it joins the next wave, including planning
work whose decisions are worth settling now; deferral does not abandon a
feature. A study may end with a measured no-go. Work gated on hardware, a
partner, a consumer, or a provider waits in [m3](/quest/m3/README.md); work
waiting on an upstream release waits in [m4](/quest/m4/README.md).

## Required

- [P2P](/quest/m2/p2p/README.md) - opted-in clients serve each other over data channels and iroh while the relay stays the rendezvous and the fallback, under application policy
- [One port](/quest/m2/one-port/README.md) - a relay speaks QUIC and STUN on one UDP port and HTTP, RTMP, and RTMPS on one TCP port; WebRTC media is an embedder hook
- [Ladder](/quest/m2/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [Processor](/quest/m2/processor/README.md) - a customer-run worker publishes an on-demand contribution under its own service prefix with scoped access
- [Stream sessions](/quest/m2/uring-tcp/README.md) - serve WebSocket and HTTP from the io_uring workers, where io_uring pays off most
- [fMP4 emsg](/quest/m2/emsg.md) - settles the shared framing and missing-data semantics before this section adopts them
- [ID3 catalog section](/quest/m2/id3.md) - timed ID3 as a first-class container-neutral catalog section
- [FLV script tags](/quest/m2/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [#2279](/quest/m2/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues arrive immediately on an independent metadata track, optionally associated with a rendition
- [CEA-608/708](/quest/m2/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [Browser archive](/quest/m2/archive-browser.md) - the same contract for browser-published broadcasts
- [Paced replay](/quest/m2/archive-paced-replay.md) - a replay pushes its groups to live subscribers on one shared clock, so any live player plays it
- [Install moq](/quest/m2/moq-installer.md) - one command installs or upgrades the released CLI on macOS and Linux
- [Install URL](/quest/m2/moq-install-url.md) - moq.dev serves the canonical installer at /install.sh
- [`moq relay`](/quest/m2/moq-relay-subcommand.md) - the relay runs under a `moq` verb with its own flags and TOML, while `moq-relay` stays a minimal binary
- [Linux decoded frames](/quest/m2/obs-decode-linux.md) - present supported native decoded surfaces with visible CPU fallback
- [Windows decoded frames](/quest/m2/obs-decode-windows.md) - present decoded D3D11 surfaces in OBS without CPU readback
- [macOS GPU input](/quest/m2/obs-macos.md) - feed the encoder from the OBS compositor without CPU readback
- [Windows GPU input](/quest/m2/obs-windows.md) - import or blit OBS D3D11 textures with explicit synchronization
- [Sans-IO IETF session](/quest/m2/rs2ts-sans-io-ietf.md) - the session shape it translates
- [Generated IETF](/quest/m2/rs2ts-ietf.md) - @moq/net's moq-transport session is generated too
- [Per-stream deadlines](/quest/m2/quic-deadline.md) - hopeless retransmits
  become resets, and a tail loss probe fires early while there is still time
- [qmux on the QUIC stream state machine](/quest/m2/quic-qmux.md) - qmux is a
  first-class crate in the fork over the shared stream state machine
- [Align BBR loss handling with draft-06](/quest/m2/quic-bbr-loss-parity.md) - losses use their own sample and undo re-enters ProbeUp through Refill
- [Measure ECN on the backbone](/quest/m2/quic-ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [Starvation at frame granularity](/quest/m2/starvation-frames.md) - the
  acknowledged frontier moves at every frame boundary through `poll_acked`,
  with a delivery-delay histogram for jitter
- [Per-stream ACK progress](/quest/m2/quic-ack-progress.md) - `moq-quic` reports
  how far a send stream has been acknowledged and when
- [poll_acked in web-transport](/quest/m2/quic-ack-hook.md) - the
  backend-neutral hook that awaits an acknowledged stream offset, implemented
  for noq and released
- [#3200](/quest/m2/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [#3202](/quest/m2/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: use fixed-file slots for worker UDP sockets
- [Open contract](/quest/m2/uring-open-contract.md) - settle concurrent ownership and backpressure before implementation
- [#3129](/quest/m2/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - moq-uring: write the WebTransport stream header at open time, so finish() never owes one
- [Cache shard](/quest/m2/cache-shard.md) - stop hammering one process-global cache line from every worker
- [Encoder feedback](/quest/m2/stats-encoder-feedback.md) - a Rust encoder
  reads its viewers' feedback and adapts its bitrate
- [Text availability](/quest/m2/text-schema.md) - a text track publishes its own coverage index instead of copying the media timeline
- [Closure counters](/quest/m2/closure-counters.md) - a departed node's return never regresses the closure counters a consumer already saw
- [Bench coverage](/quest/m2/bench-coverage.md) - Criterion targets for moq-pattern matching first, then the stats producer, moq-mux containers, the hang catalog, and moq-auth
- [Signed priority](/quest/m2/signed-priority.md) - on dev, every API priority is an `i8` with 0 as the unset midpoint, and hang's built-ins sit above it
- [AV1 metadata separation](/quest/m2/av1-metadata.md) - retain metadata OBUs inline while evaluating separate delivery
- [Catalog track identity](/quest/m2/catalog-tracks.md) - a changed track configuration becomes a new track name or epoch, never a mutated definition
- [Catalog colour model](/quest/m2/color-catalog.md) - the catalog describes a rendition's colour and HDR properties once a renderer consumes them
- [Archive recovery listing](/quest/m2/archive-recovery-listing.md) - a resumed DVR lists what changed since its checkpoint, not every stored group
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
- [#2819](/quest/m2/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: validate PipeWire DMA-BUFs into the Vulkan renderer on hardware, and export V4L2 buffers as DMA-BUFs
- [vcpkg registry](/quest/m2/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests
- [Binary delta stats](/quest/m2/stats-delta.md) - an on-demand varint delta flavor of every stats track, if relay encode CPU still matters after the JSON fixes
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - moq-uring receives from the buffer ring and transmits into registered buffers with no copy, once a profile says where
- [BBR media study](/quest/m2/quic-bbr-natural-drain.md) - whether bounded drain credit avoids ProbeRTT deadline interference, and where our BBR differs from Google's
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [L4S on the backbone](/quest/m2/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/m2/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive by deadline](/quest/m2/quic-keep-alive.md) - a PING only when the idle deadline nears, no fixed timer
- [Socket close](/quest/m2/noq-socket-close.md) - `moq-quic` releases an endpoint's socket on close, so moq-tokio drops its wrapper
- [GOP overhead](/quest/m2/gop-overhead.md) - price the I-frames a short GOP pays for, deciding whether a long GOP plus a keyframe request is worth designing
- [Per-program SI](/quest/m2/ts-program-si.md) - a selected TS program's broadcast carries only its own service's SDT and EIT
- [TS import health](/quest/m2/ts-import-health.md) - `moq import ts` counts the TR 101 290 errors of the feed it receives, PCR and PTS graded on its own values
- [TS export liveness](/quest/m2/ts-export-liveness.md) - `moq export ts` reports each elementary stream's access units and quiet time, catching a per-track stall
- [TS health stats](/quest/m2/ts-health-stats.md) - the TS counters ride the stats plumbing beside the media counters
- [Teleoperation](/quest/m2/teleop/README.md) - MoQ carries robot video down and control up on one session as a library capability
- [Media QA on other engines](/quest/m2/browser-media-qa-engines.md) - the media harness measures a Firefox or WebKit player over the fallback and names what each engine lacks
- [Windows.Graphics.Capture](/quest/m2/capture-wgc.md) - one WGC backend for display and window capture with the cursor, replacing Desktop Duplication and GDI
- [Windows capture parity](/quest/m2/capture-windows.md) - system audio and a settled app-capture policy
- [Linux capture parity](/quest/m2/capture-linux.md) - Wayland window/system-audio capture with explicit display-selection and app-capture limits
- [Audio capture time](/quest/m2/audio-capture-time.md) - native audio stamps a buffer's capture instant, not when the driver reads it
- [X11 capture transport](/quest/m2/x11-capture-shm.md) - move X11 capture to shared memory and RandR events instead of a per-frame socket copy
- [Egress profile](/quest/m2/quic-egress-profile.md) - measure relay send-path syscalls, pacing bursts, and allocations before optimizing any of them
- [SEI separation study](/quest/m2/sei.md) - measure whether separating SEI saves enough, or has a metadata-only consumer, to justify a split
- [Compressed tracks](/quest/m2/flate.md) - moq-ffi and every wrapper expose flate tracks through a `flate` namespace like `json`
- [Announcement shapes](/quest/m2/announce-shapes.md) - moq-lite announcements and interests carry prefix, exact, suffix, or prefix+suffix shapes that survive relay hops, benchmarked over the announce table
- [moq-transport cluster peers](/quest/m2/ietf-cluster-peers.md) - an extended cluster draft lets moq-transport relays peer again, stitching failover on the reply's origin
- [MSFTS convergence](/quest/m2/msfts-convergence.md) - the demultiplexed TS lane converges on MSFTS where the two still differ: program tables and the ES payload unit
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - H.265 encode and decode, pre-generated bindings, and pooled resize surfaces, including the moq-dev/vaapi release that carries them
