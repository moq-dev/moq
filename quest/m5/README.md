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

Pruned 2026-11-09 (maintainer, /quest-plan from the deletion audit): 80
quests with no named consumer, no outside gate, and no m0 to m4 quest
requiring them were deleted; git history keeps them. Consumers were checked
in this repository and in moq.pro's plans, which kept the io_uring tuning
quests (fleet relays on io_uring workers), broadcast fairness, and the
transcode and NVENC quests (production transcoding). Re-plan one when a
consumer asks.

## Required

- [Miri's GenMC mode explores every schedule of kio's lock-free code](/quest/m5/miri-genmc.md) - waiting on rust-lang/miri to run `cargo miri test` in GenMC mode, then Nightly checks kio's log and channel exhaustively
- [Mesh over ICE](/quest/m5/mesh-ice.md) - mesh nodes on different networks connect directly through the P2P line's ICE pairs, once a mesh user needs it
- [Video validation hardware is on hand](/quest/m5/video-hardware-access.md) - the machines the validation runs on, including a Raspberry Pi 4/5 for embedded presenting
- [Fair share across broadcasts on cluster links](/quest/m5/broadcast-fairness.md) - broadcasts share a cluster link fairly, so one tenant's priority never starves another's, once a tenant identity exists
- [Video hardware validation](/quest/m5/video-hardware.md) - run the encode, capture, and zero-copy paths that were never run on real machines, including PipeWire on KDE, the camera portal, and a Pi
- [An Ada NVIDIA GPU is available](/quest/m5/ada-gpu.md) - the hardware AV1 NVENC is verified on
- [NVENC AV1](/quest/m5/nvenc-av1.md) - AV1 encode through NVENC, once an Ada-generation GPU is available
- [NVDEC and NVENC share CUDA arrays](/quest/m5/nvenc-cuda-arrays.md) - SDK 13.1 application-owned CUDA arrays drop the per-frame device copy, on driver 610+
- [Embedded video](/quest/m5/video-embedded.md) - verify a Pi 4/5 presents through the existing Vulkan and CPU paths, adding EGL import only if that fails
- [A physical-NIC peer for the zero-copy sweep](/quest/m5/zero-copy-peer.md) - the rig the zero-copy sweep runs on
- [#3201: moq-uring: use SENDMSG_ZC for large UDP GSO trains](/quest/m5/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - complete the prerequisite issue first
- [#3204](/quest/m5/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [Cluster dials on the ring](/quest/m5/uring-cluster-dial.md) - measure whether an io_uring relay's outbound cluster links belong on its workers
- [#3202](/quest/m5/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: fixed-file slots for worker UDP sockets, if its own benchmark measures a win
- [Byte streams in moq-ffi](/quest/m5/ffi-stream.md) - Swift and Kotlin dial and accept byte streams, for moq.pro's T3 Code mobile client
- [E2EE in moq-ffi](/quest/m5/ffi-e2ee.md) - Swift and Kotlin publish and consume protected broadcasts, for the same client
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
- [Interop runner withdraws before disconnecting](/quest/m5/interop-runner-approval.md) - condition: a released moq-tokio and @moq/net carry close() withdrawals; then the runner's moq.dev clients switch to close()
- [Safari ships the WebKit 319818 fix](/quest/m5/webkit-319818.md) - the Safari release the gate admits
- [Safari WebTransport](/quest/m5/safari-webtransport.md) - WebKit browsers return to WebTransport once WebKit 319818 ships fixed
- [moq-video in the browser](/quest/m5/video-web.md) - getUserMedia, getDisplayMedia, WebCodecs, and VideoFrame import into wgpu, for moq-dev/studio's web studio
- [moq-audio in the browser](/quest/m5/audio-web.md) - AudioWorklet capture and playback and WebCodecs Opus and AAC, for the web studio's audio
- [Mesh keys](/quest/m5/mesh-keys.md) - parked: encrypted and HMAC-authenticated mesh broadcasts through a wrapped `.keys` track
- [rust2-ts reaches moq-net's model](/quest/m5/rs2ts-model-ready.md) - rust2's translator is ready to translate the model
- [Keep moq-net's model in rust2-ts's subset](/quest/m5/rs2ts-model-subset.md) - safe twins for the model's internal `unsafe`, `dyn Expiry` as an enum, once rust2-ts reaches the model
- [Wait for a stable bun lint](/quest/m5/bun-lint-release.md) - condition: `bun lint` ships in a stable Bun that nixpkgs carries
- [A transcode ladder probes an unseen source shape without stalling its other rungs](/quest/m5/transcode-parallel-open.md) - a source changing to an unseen size opens the new encoders without stalling the ladder's other rungs
- [Relay ACME on instant-acme](/quest/m5/relay-acme.md) - moq-relay's ACME returns on instant-acme and rind, once a user needs it
- [Native compression flush](/quest/m5/native-compression-flush.md) - report the per-frame nonfinal flush requirement in existing browser discussions
- [Native synchronous codecs](/quest/m5/native-compression-sync.md) - report the stateful small-frame synchronous API requirement
- [Native browser codecs](/quest/m5/native-compression-adopt.md) - adopt native operations when timing, frame and resource contracts hold
- [Vendor-free CARLA bridge](/quest/m5/carla-bridge.md) - the CARLA demo's GPU bridge lets moq-video pick the encoder, and its stand-in test runs on AMD as well as NVIDIA
- [Wrapper traits over moq-net](/quest/m5/net-wrap/README.md) - moq-net exports producer and consumer traits, and every writer and reader above it takes them, so a wrapper such as e2ee passes through unchanged
- [E2EE](/quest/m5/e2ee/README.md) - TypeScript and Rust peers interoperate over encrypted broadcasts no relay can decrypt
- [Publishers learn their cap](/quest/m5/rate-grant.md) - the AUTH grant carries the caps and publishers clamp their encoder to them
- [WASM-to-JS evaluation](/quest/m5/rs2ts-wasm2js.md) - try the Rust implementation as JS-only browser output and compare source-translator alternatives if needed
- [JS-only @moq/net](/quest/m5/rs2ts-js.md) - adopt the selected Rust-to-JS route after browser parity and measured tradeoffs are accepted
- [#3199](/quest/m5/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup, if a profile shows the win
- [#3200](/quest/m5/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [Browser media report](/quest/m5/rs2ts-browser-report.md) - compare Rust-backed and handwritten @moq/net through full media workloads, supplementing the pre-adoption comparisons
- [Link quality](/quest/m5/link-quality.md) - a radio link's cost follows its measured quality without flapping routes
- [io_uring thread check](/quest/m5/uring-transport-machines.md) - the relay fanout bench measures `moq_uring::transport::Local`'s per-call thread check after the transport is boxed, removing it only if it shows
- [NVENC buffer pool](/quest/m5/nvenc-pool.md) - NVENC reuses input and output buffers instead of allocating per frame, if a benchmark shows it wins
- [QUIC I/O boundary](/quest/m5/quic-io-boundary.md) - moq-uring receives from the buffer ring and transmits into caller-owned buffers with no copy, once a profile says where
- [A failed archive time lookup fails the subscription](/quest/m5/archive-time-followups.md) - when an `archive::Consumer` cannot answer a `Time(pts)` lookup because a store GET fails or an object does not decode, the subscription fails with a typed error naming the lookup failure instead of silently falling back to the cache
