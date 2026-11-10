# m5: deferred

## Goal

Work that waits on something besides priority: either a first step outside
this repository (hardware nobody on the team has, a partner or customer who
asks for it, a hosting provider's offer, or an upstream release), or a
speculative feature or study with no named consumer yet.

## Plan

A quest gated on the outside world has a condition quest beside it, as the
root [questline](/quest/README.md) describes. A speculative feature or study
with no consumer needs no condition quest: it parks here rather than in m3 or m4
until a consumer appears, and is deleted when it goes stale; git history keeps
it. Decided in the 2026-10-05 audit: both kinds live here, and the m2 quests
deferred for having no named consumer moved here. The 2026-10-08 audit folded
m4's upstream waits in here and moved m2 work with no consumer.

Decided 2026-11-05 (maintainer, milestone re-cut): this milestone was m3 and
is now m5, after m3 and m4 took the two halves of the old m2. The old m2's
features, studies, and optimizations with no named consumer moved here, per
the rule above, unless an m0 to m4 quest requires them. It stays the deferred
bucket when milestones shift after a cut.

Rate-grant work targets the next unpublished lite version after finalized
lite-07; bitrate enforcement is not part of the next release.

## Required

- [Mesh over ICE](/quest/m5/mesh-ice.md) - mesh nodes on different networks connect directly through the P2P line's ICE pairs, once a mesh user needs it
- [Video validation hardware is on hand](/quest/m5/video-hardware-access.md) - the machines the validation runs on, including a Raspberry Pi 4/5 for embedded presenting
- [Fair share across broadcasts on cluster links](/quest/m5/broadcast-fairness.md) - broadcasts share a cluster link fairly, so one tenant's priority never starves another's, once a tenant identity exists
- [Video hardware validation](/quest/m5/video-hardware.md) - run the encode, capture, and zero-copy paths that were never run on real machines, including PipeWire on KDE, the camera portal, and a Pi
- [An Ada NVIDIA GPU is available](/quest/m5/ada-gpu.md) - the hardware AV1 NVENC is verified on
- [NVENC AV1](/quest/m5/nvenc-av1.md) - AV1 encode through NVENC, once an Ada-generation GPU is available
- [#2147](/quest/m5/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md) - NVIDIA Main10 encode, once a 10-bit source exists in the pipeline
- [NVDEC and NVENC share CUDA arrays](/quest/m5/nvenc-cuda-arrays.md) - SDK 13.1 application-owned CUDA arrays drop the per-frame device copy, on driver 610+
- [Embedded video](/quest/m5/video-embedded.md) - verify a Pi 4/5 presents through the existing Vulkan and CPU paths, adding EGL import only if that fails
- [Multi-plane PipeWire cameras](/quest/m5/pipewire-camera-planes.md) - I420 and NV12 cameras that deliver one memory block per plane capture
- [Video codec coverage](/quest/m5/video-codec-coverage.md) - prioritize remaining native AV1 and portable decoder gaps once a consumer asks
- [A physical-NIC peer for the zero-copy sweep](/quest/m5/zero-copy-peer.md) - the rig the zero-copy sweep runs on
- [#3201: moq-uring: use SENDMSG_ZC for large UDP GSO trains](/quest/m5/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - complete the prerequisite issue first
- [#3204](/quest/m5/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [Cluster dials on the ring](/quest/m5/uring-cluster-dial.md) - measure whether an io_uring relay's outbound cluster links belong on its workers
- [#3202](/quest/m5/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: fixed-file slots for worker UDP sockets, if its own benchmark measures a win
- [L4S on the backbone](/quest/m5/quic-ecn.md) - an ECT(1) option in `moq-quic`, an `ecn` config knob, and a dualpi2 measurement, if the ECN measurement says marks survive
- [Careful resume on reconnect](/quest/m5/quic-careful-resume.md) - a Rust redial starts at the previous connection's rate
- [Cut-through](/quest/m5/cut-through/README.md) - a bufferbloat bench decides whether a relay should forward bytes behind a QUIC stream hole; the build is re-planned if it says go
- [Byte streams in moq-ffi](/quest/m5/ffi-stream.md) - Swift and Kotlin dial and accept byte streams, for moq.pro's T3 Code mobile client
- [E2EE in moq-ffi](/quest/m5/ffi-e2ee.md) - Swift and Kotlin publish and consume protected broadcasts, for the same client
- [Unreal prototype](/quest/m5/unreal.md) - a UE5 module on the C++ package with exceptions disabled, rendering a subscribed broadcast to a texture
- [vcpkg registry](/quest/m5/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests, once a consumer asks
- [Conan remote](/quest/m5/cpp-conan.md) - a remote we own serves the same tarball to `conan install`
- [Linux OBS GPU input](/quest/m5/obs-linux-gpu.md) - publish OBS compositor frames without CPU readback on a validated Linux graphics/encoder combination
- [Routing cost domains](/quest/m5/routing-cost-domains.md) - design operator boundaries and policy without adding incomparable costs
- [Request tokens beyond Rust](/quest/m5/request-token-surfaces.md) - a JS, moq-ffi, or moq-cli request-token API, built only for the surface a consumer names
- [Token on FETCH](/quest/m5/auth-token-fetch.md) - our TRACK_STATUS and FETCH carry the request token, once a consumer asks
- [moq-transport cluster peers](/quest/m5/ietf-cluster-peers.md) - the cluster draft carries the route layer, so moq-transport relays peer again, once a moq-transport relay operator asks
- [The moq.pro mesh runs lite-07](/quest/m5/lite07-mesh.md) - the deployment that makes the exemption dead code
- [Drop the hidden cluster exemption](/quest/m5/hidden-exemption.md) - relays stop forcing hidden broadcasts on cluster peers once every peer opts in on the wire
- [Robot teleoperation conventions](/quest/m5/teleop-robot.md) - documented track shapes for video down and control up, implemented by the MAVLink gateway, once a robotics consumer appears
- [Operator arbitration](/quest/m5/teleop-arbitration.md) - one operator holds control at a time
- [MAVLink bridge](/quest/m5/teleop-mavlink.md) - a `moq-mavlink` gateway
  replacing the VPN plus two unmanaged UDP flows, with QGroundControl and
  friends unchanged
- [SITL proof and browser ground station](/quest/m5/teleop-proof.md) - ArduPilot
  SITL and a synthetic camera flown from a browser ground station, reproducible
  in five minutes
- [Ladder](/quest/m5/ladder/README.md) - a transcode ladder adapts to the uplink it publishes over, instead of encoding every live rung at its ceiling
- [fMP4 emsg](/quest/m5/emsg.md) - settles the shared framing and missing-data semantics before this section adopts them
- [ID3 in the events section](/quest/m5/id3.md) - timed ID3 as a container-neutral scheme in the catalog's `events` section
- [FLV script tags](/quest/m5/flv-script.md) - onMetaData and AMF data messages survive RTMP and FLV import
- [Half and quarter rate AC-3](/quest/m5/ac3-low-rate.md) - bsid 9 and 10 AC-3 parse through TS and FLV instead of being refused
- [MSF feedback tracks](/quest/m5/msf-publish-tracks.md) - `--report` and `--echo` work under `--catalog-format msf` through `publishTracks`, once a consumer asks
- [MSFTS convergence](/quest/m5/msfts-convergence.md) - the demultiplexed TS lane converges on MSFTS where they differ, once an MSFTS implementer engages
- [#2279](/quest/m5/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - hang: SCTE-35 cues arrive immediately on an independent metadata track, optionally associated with a rendition
- [CEA-608/708](/quest/m5/captions-cea.md) - captions carried inside video SEI become a real text rendition at import
- [WebXR playback](/quest/m5/watch-webxr.md) - `<moq-watch>` renders equirectangular and stereo renditions through WebXR layers on a headset
- [Paced replay](/quest/m5/archive-paced-replay.md) - an archive consumer pushes stored groups to live subscribers on one shared clock, so any live player plays it
- [Encoder feedback](/quest/m5/stats-encoder-feedback.md) - a Rust encoder
  reads its viewers' feedback and adapts its bitrate
- [Interop runner withdraws before disconnecting](/quest/m5/interop-runner-approval.md) - condition: a released moq-tokio and @moq/net carry close() withdrawals; then the runner's moq.dev clients switch to close()
- [Safari ships the WebKit 319818 fix](/quest/m5/webkit-319818.md) - the Safari release the gate admits
- [Safari WebTransport](/quest/m5/safari-webtransport.md) - WebKit browsers return to WebTransport once WebKit 319818 ships fixed
- [moq-video in the browser](/quest/m5/video-web.md) - getUserMedia, getDisplayMedia, WebCodecs, and VideoFrame import into wgpu, for moq-dev/studio's web studio
- [moq-audio in the browser](/quest/m5/audio-web.md) - AudioWorklet capture and playback and WebCodecs Opus and AAC, for the web studio's audio
- [Mesh keys](/quest/m5/mesh-keys.md) - parked: encrypted and HMAC-authenticated mesh broadcasts through a wrapped `.keys` track
- [rust2-ts reaches moq-net's model](/quest/m5/rs2ts-model-ready.md) - rust2's translator is ready to translate the model
- [Keep moq-net's model in rust2-ts's subset](/quest/m5/rs2ts-model-subset.md) - safe twins for the model's internal `unsafe`, `dyn Expiry` as an enum, once rust2-ts reaches the model
- [Wait for a stable bun lint](/quest/m5/bun-lint-release.md) - condition: `bun lint` ships in a stable Bun that nixpkgs carries
- [Rust and JS end a lite-05 track the same way after an inclusive SUBSCRIBE_END](/quest/m5/lite05-subscribe-end.md) - after a legacy inclusive SUBSCRIBE_END, Rust and JS both end the track instead of Rust sending a FIN and JS only releasing
- [Byte-at-a-time MKV input imports as fast as chunked input](/quest/m5/mkv-input-coalesce.md) - the MKV importer fed one byte at a time is no slower than before moq-dev/moq#7234
- [A transcode ladder probes an unseen source shape without stalling its other rungs](/quest/m5/transcode-parallel-open.md) - a source changing to an unseen size opens the new encoders without stalling the ladder's other rungs
- [Relay ACME on instant-acme](/quest/m5/relay-acme.md) - moq-relay's ACME returns on instant-acme and rind, once a user needs it
- [Announce credit](/quest/m5/announce-credit.md) - a subscriber bounds an Announce Stream's routes with credit at the wire, so a path flood never closes a session
- [A C origin can discover eagerly, like a relay](/quest/m5/c-origin-relay.md) - the C API puts an origin in relay mode, so forwarding a client session's routes needs no local announce watcher
- [Native compression flush](/quest/m5/native-compression-flush.md) - report the per-frame nonfinal flush requirement in existing browser discussions
- [Native synchronous codecs](/quest/m5/native-compression-sync.md) - report the stateful small-frame synchronous API requirement
- [Native browser codecs](/quest/m5/native-compression-adopt.md) - adopt native operations when timing, frame and resource contracts hold
- [Vendor-free CARLA bridge](/quest/m5/carla-bridge.md) - the CARLA demo's GPU bridge lets moq-video pick the encoder, and its stand-in test runs on AMD as well as NVIDIA
- [Performance survey follow-ups](/quest/m5/performance-survey/README.md) - benchmark the source audit's transport, browser, media and serving risks before optimizing
- [Receive pool](/quest/m5/receive-pool.md) - one byte budget per worker covers every connection's receive credit and the cache, billing each frame once
- [Front parking](/quest/m5/origin-front-parks.md) - an unroutable request waits on a front instead of re-asking on every route-table move
- [A route stream's interest is a set of prefixes](/quest/m5/route-interest-prefixes.md) - one Announce Stream covers several subtrees, with floors and certification proved over the set
- [Fewer allocations in announce fan-out](/quest/m5/announce-fanout-allocs.md) - a deep path costs no more allocations per subscriber than a shallow one
- [moq-room scales to large rooms](/quest/m5/room-bench.md) - a benchmark measures `Room` with large participant counts, and polling narrows only if it shows a slope
- [Route health](/quest/m5/route-health/README.md) - epoch-only active/passive track recovery over metadata-only subscriptions
- [Wrapper traits over moq-net](/quest/m5/net-wrap/README.md) - moq-net exports producer and consumer traits, and every writer and reader above it takes them, so a wrapper such as e2ee passes through unchanged
- [E2EE](/quest/m5/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [Bindings size profile](/quest/m5/ffi-size-profile.md) - a benchmark decides whether the moq-ffi builds ship at opt-level "s", which halves the dylib
- [Value merge-patch diff](/quest/m5/json-value-diff.md) - benchmarks decide between moq-json's generic serializer diff, a ported `Value` diff, and compression alone
- [Measure what a catalog republish costs per rendition edit](/quest/m5/catalog-republish-cost.md) - bench whole-catalog re-serialization across track counts, then decide deltas or a bound
- [WebGPU HDR](/quest/m5/webgpu-hdr.md) - HDR renditions play as HDR where the browser and display can show it, and tone-map to SDR elsewhere
- [moq-transport hole markers](/quest/m5/ietf-hole-marker.md) - a negotiated extension names a SUBSCRIBE's holes, so our publishers answer multi-group FETCH from caches that know every group; built when an IETF client wants it
- [A parked read of a resumed group reads its budget without re-syncing](/quest/m5/recover-budget.md) - measure, then drop the per-frame reader sync
- [Publishers learn their cap](/quest/m5/rate-grant.md) - the AUTH grant carries the caps and publishers clamp their encoder to them
- [Linux decoded frames](/quest/m5/obs-decode-linux.md) - present supported native decoded surfaces with visible CPU fallback
- [Windows decoded frames](/quest/m5/obs-decode-windows.md) - present decoded D3D11 surfaces in OBS without CPU readback
- [macOS GPU input](/quest/m5/obs-macos.md) - feed the encoder from the OBS compositor without CPU readback
- [Windows GPU input](/quest/m5/obs-windows.md) - import or blit OBS D3D11 textures with explicit synchronization
- [WASM-to-JS evaluation](/quest/m5/rs2ts-wasm2js.md) - try the Rust implementation as JS-only browser output and compare source-translator alternatives if needed
- [JS-only @moq/net](/quest/m5/rs2ts-js.md) - adopt the selected Rust-to-JS route after browser parity and measured tradeoffs are accepted
- [Per-stream and per-datagram deadlines](/quest/m5/quic-deadline.md) - hopeless retransmits
  become resets, a tail loss probe fires early while there is still time, and
  datagrams past their subscription's max delay are dropped instead of sent late
- [Measure ECN on the backbone](/quest/m5/quic-ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [Delivery delay](/quest/m5/delivery-delay.md) - a per-frame
  delivery-delay histogram from `poll_acked`, so viewer jitter is visible
- [#3199](/quest/m5/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup, if a profile shows the win
- [#3200](/quest/m5/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [Browser media report](/quest/m5/rs2ts-browser-report.md) - compare Rust-backed and handwritten @moq/net through full media workloads, supplementing the pre-adoption comparisons
- [Link quality](/quest/m5/link-quality.md) - a radio link's cost follows its measured quality without flapping routes
- [io_uring thread check](/quest/m5/uring-transport-machines.md) - the relay fanout bench measures `moq_uring::transport::Local`'s per-call thread check after the transport is boxed, removing it only if it shows
- [Opus implementation](/quest/m5/audio-opus-backend.md) - compare Opus codec quality, CPU, build cost, and the loss recovery each backend offers
- [Latency ledger](/quest/m5/latency-ledger.md) - a viewer reports its share of playback delay stage by stage: jitter buffer and device
- [MediaCodec decode](/quest/m5/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m5/audio-encode-mediacodec.md) - Android encodes AAC-LC
- [#2848](/quest/m5/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the Opus producer follows its bandwidth grant through the settled `moq_mux::rate::Control`
- [NVENC buffer pool](/quest/m5/nvenc-pool.md) - NVENC reuses input and output buffers instead of allocating per frame, if a benchmark shows it wins
- [QUIC I/O boundary](/quest/m5/quic-io-boundary.md) - moq-uring receives from the buffer ring and transmits into caller-owned buffers with no copy, once a profile says where
- [BBR media study](/quest/m5/quic-bbr-study.md) - whether Google's growth recognition and precautionary probe stop help media, measured on a shaped media harness
- [Egress profile](/quest/m5/quic-egress-profile.md) - measure relay send-path syscalls, pacing bursts, and allocations before optimizing any of them
- [Archive lookups cancel when abandoned](/quest/m5/archive-lookup-withdrawal.md) - a public withdrawal signal on `Fill` stops a time lookup's store read once its readers leave
- [Demo dashboard shows viewer lag](/quest/m5/qos-lag-dashboard.md) - follow-on capability after the release contract is stable
- [A failed archive time lookup fails the subscription](/quest/m5/archive-time-followups.md) - when an `archive::Consumer` cannot answer a `Time(pts)` lookup because a store GET fails or an object does not decode, the subscription fails with a typed error naming the lookup failure instead of silently falling back to the cache
- [HTTP auth servers see which request named a token](/quest/m5/auth-http-request-kind.md) - the HTTP auth contract forwards `auth::Request`'s `kind()` and `path()`
- [moq auth sign mints capped tokens](/quest/m5/auth-sign-caps.md) - CLI flags for the bitrate caps
- [QUIC-LB forwarding reads the cluster's peers and reports its counts](/quest/m5/quic-lb-forward-extras.md) - a cluster-API peer map and forward counters
