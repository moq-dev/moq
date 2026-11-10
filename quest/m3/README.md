# m3: later work, higher priority

## Goal

Work that follows the next breaking release and has someone waiting on it:
byte streams and the mesh libraries first, then release execution after the
cut, quests a named consumer needs, including downstream deployments, and bug fixes in
shipped code. Breaking and additive work mix here; the cut after m2's sorts
them by kind.

## Plan

Decided 2026-11-05 (maintainer, milestone re-cut): the old m2 ("later work")
split in half by priority into m3 and m4. A quest joined m3 when a named
consumer or user-facing need waits on it, when it is release execution or a
bug fix in shipped code, or when an m0 to m3 quest requires it. A downstream
deployment's plans count as a consumer: work its near-term plans need joins
m3, work only its urgent plans need joins m1 or m2 by kind, and work only its
later plans need joins m4. The recurring hygiene audits live here behind the
cut, so the shift after the cut brings each into m1, as before.

Decided 2026-10-13: a feature does not block the breaking release merely
because it was in the next-wave backlog. Keep existing behavior correct in
m2, and defer new capabilities without changing their intended outcome.
Dependencies on m1 and m2 remain explicit. An implementation that discovers
a new necessary API break brings that decision to the maintainer before
proceeding.

Decided 2026-11-03 (maintainer, Kixel's quest audit): [byte streams](/quest/m3/stream/README.md)
and the [mesh libraries](/quest/m3/mesh/README.md) rank first here, ahead
of [release execution](/quest/m3/release/README.md). Kixel's daemon builds on
them from `main` through a git submodule, so it needs them merged, not
released. They stay out of m1 and m2 so the cut does not wait on them; each
starts as soon as its prerequisites (sockets, fastest-reader pacing, outbound
federation) land.

## Required

- [Byte streams](/quest/m3/stream/README.md) - TCP connections ride MoQ losslessly through relays, proxied per hop rather than as IP packets
- [Mesh libraries](/quest/m3/mesh/README.md) - an `Endpoint` reaches any enrolled node by endpoint ID over relays or a sparse direct overlay, with address hints and a `.status` track; the mesh binary and UI live in Kixel
- [`@moq/net` broadcasts expose their requests](/quest/m3/net-broadcast-requests.md) - a JS broadcast serves tracks it never created, on demand, like Rust
- [`@moq/crypto` loads a stored signing key](/quest/m3/crypto-key-load.md) - one device key proves mesh identity and signs broadcasts
- [JS announced coalescing](/quest/m3/js-announced-coalesce.md) - `@moq/net`'s `Announced` keeps one pending event per path, like Rust
- [An audio track's final flush joins its open group](/quest/m3/audio-flush-group.md) - a zero-`delay.max` subscriber gets the tail, not `Old`
- [Release execution](/quest/m3/release/README.md) - publication, installed-artifact proof, cleanup, and downstream adoption after m2
- [Vendored code has every upstream fix it needs](/quest/m3/vendor-sync/README.md) - the next cycle's vendor sync, which the cut moves under the m2 cut gate
- [Release pull requests read the R2 Rust cache](/quest/m3/mbx-r2-release-cache.md) - after the cut brings the platform Rust cache to `release`
- [Binary release publishers run on their tag](/quest/m3/release-tag-publishers.md) - after the cut brings the workflows to `release`
- [Relays split horizon per session](/quest/m3/route-split-horizon.md) - a relay never advertises a route back over the session it came from
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - a relay stays on a broadcast's path only for a local reader, two downstream sessions, or its one consumer's cheapest cold route, and joins a neighbor's copy instead of pulling another
- [crates.io trusts the workflow](/quest/m3/crates-trusted-publisher.md) - maintainer action: the crates the cut first-publishes by token get a trusted publisher for `release-rs.yml`
- [Rust releases publish through OIDC](/quest/m3/release-rs-oidc.md) - release-plz publishes with trusted publishing and `CARGO_REGISTRY_TOKEN` is revoked
- [One policy for oversized container metadata](/quest/m3/import-metadata-policy.md) - fMP4 and MKV refuse or stream huge metadata by one documented rule
- [Video quality](/quest/m3/video-quality/README.md) - a nightly VMAF harness grades every encoder backend and on-demand GoP pops against floors, and moq-boy's encode is fixed under it
- [rust2's exporter is ready for moq](/quest/m3/rust2-exporter.md) - waiting on moq-dev/rust2's exporter to generate every language moq ships
- [Switch to rust2's exporter](/quest/m3/ffi-exporter.md) - moq's bindings come from rust2-ffi, moq-ffi shrinks to its remainder, and moq's UniFFI generator code is deleted
- [Generated C bindings](/quest/m3/c/README.md) - C generated from moq-ffi ships as `moq-c` 0.8.0 and replaces the hand-written crate
- [A runtime-free QUIC driver](/quest/m3/quic-sansio-driver.md) - a caller's own loop moves UDP datagrams and polls moq sessions over moq-quic, for rust2's bindings
- [QoS](/quest/m3/qos/README.md) - the relay's egress `lag` histogram and `dropped` counters on the stats totals and prefix tracks
- [The DASH manifest keeps segments across a reconfigure](/quest/m3/dash-reconfigure.md) - a same-codec reconfigure keeps the old instance's segments listed in DASH, as HLS does
- [Plan: watch worker](/quest/m3/plan-watch-worker.md) - prototype an invisible page worker against app-spawned workers, and land the jank harness that decides
- [Watch worker](/quest/m3/watch-worker.md) - watch playback runs in a worker onto an OffscreenCanvas, so main-thread jank never stalls video or audio
- [Vulkan Video encode on AMD](/quest/m3/vulkan-encode.md) - H.264 and H.265 from an external Vulkan image on RADV, hand-rolled on ash, proven on an RX 9070
- [H.265 encodes respect the encoder's minimum size](/quest/m3/h265-min-width.md) - an H.265 encode below AMD's 384-wide minimum is skipped by `Kind::Auto`, and the error names it
- [VA-API encodes an external Vulkan image](/quest/m3/vaapi-vulkan-import.md) - an explicit-sync DMA-BUF reaches Intel's encoder, proven on Arrow Lake
- [C++ FFI reuse](/quest/m3/cpp-ffi-reuse.md) - C++ packages link the shared default-feature FFI build once it is proven compatible
- [OBS native codecs](/quest/m3/obs-moq-video/README.md) - replace FFmpeg video and audio decoding with moq-video and moq-audio, deliver GPU frames, and use native audio/video encoders
- [MoQ in obs-studio](/quest/m3/obs-studio/README.md) - a native MoQ output and service merged into obs-studio, once its maintainers agree; the plugin carries it until then
- [AudioToolbox decode](/quest/m3/audio-decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [AudioToolbox encode](/quest/m3/audio-encode-audiotoolbox.md) - macOS and iOS encode AAC-LC, for moq-dev/studio's RTMP destinations
- [Media Foundation decode](/quest/m3/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m3/audio-encode-mediafoundation.md) - Windows encodes AAC-LC, for moq-dev/studio's RTMP destinations
- [GPU CI](/quest/m3/gpu-ci.md) - NVIDIA tests gate the merge queue on a self-hosted GPU runner VM, and `just rs nvidia` runs them locally instead of skipping
- [Media stats](/quest/m3/stats/README.md) - publishers announce a stats track in the catalog, viewers answer a soliciting catalog through a per-catalog `.echo` broadcast, and one model turns both into a health verdict and a preflight report
- [Wire stats](/quest/m3/wire-stats/README.md) - `Presence` counts each root's transport bytes on the wire, `wire_tx` and `wire_rx`, from QUIC, WebSocket, and embedder gateway sources
- [Session outcome records](/quest/m3/session-outcome-records.md) - relay stats publish each refusal and abnormal session end as a record per label, bounded under a flood, beside the counters
- [Audio renditions carry a language](/quest/m3/audio-lang.md) - audio renditions have an optional BCP-47 `lang`, and watch auto-picks within one language
- [fMP4 and TS imports publish BCP-47 languages](/quest/m3/import-lang-iso639.md) - ISO 639-2 codes become `en`-style tags on audio and text
- [HLS carries every audio language](/quest/m3/hls-audio-languages.md) - every `EXT-X-MEDIA` audio entry imports with its `LANGUAGE`, and exports write it back
- [A refused follower asks a replacement handler](/quest/m3/follow-refused-retry.md) - a same-instance handler swap with identical route details re-asks a refused `Following`, while listeners still see no change
- [iOS capture](/quest/m3/mobile-capture-ios.md) - Rust captures the camera and screen on iOS, setting the catalog rotation
- [Android capture](/quest/m3/mobile-capture-android.md) - Rust captures through NDK/JNI on Android, reusing the existing codecs and setting the catalog rotation
- [Latest readers use Range::live()](/quest/m3/readers-range-live.md) - after #6278, readers start at the live edge without waiting on `largest()`
- [PulseAudio capture periods](/quest/m3/pulseaudio-capture-period.md) - capture on cpal's PulseAudio host arrives in short periods, not the server's 2 s default fragment, proven on the null sink's monitor
- [A JSON snapshot consumer resyncs after a bad frame](/quest/m3/json-snapshot-resync.md) - a decode failure drops the group and the consumer resumes at the next snapshot by itself, so `@moq/room` and moq-boy keep reading
- [Go module proxy](/quest/m3/go-module-proxy.md) - `moq.dev/moq-ffi` ships from a static GOPROXY we host, so its staticlibs stop growing git history by ~61 MiB per release
- [Go per-platform lib modules](/quest/m3/go-ffi-platform-modules.md) - a Go build downloads only the host's staticlib, and `go mod vendor` works
- [Dart on iOS](/quest/m3/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can
- [Mobile completion](/quest/m3/mobile-completion.md) - verify the Swift and Kotlin bindings over Rust-owned capture
- [Migrated sessions close once their replacement synced](/quest/m3/goaway-migration-followups.md) - moq-tokio holds the early close for the replacement's announce sync, and the JS `Connection` closes early too
- [moq-transport requests end through one lease](/quest/m3/ietf-request-lease.md) - every IETF request ends through one polled lease, and the two 18k-line files split by mechanism
- [A moq-transport group head fills its hole in place](/quest/m3/ietf-fill-refill.md) - fills and joining FETCHes write into the tail's group, deleting the `Fill` handoff
- [One port](/quest/m3/one-port/README.md) - one UDP port carries QUIC, SRT, and WebRTC media through an embedder hook, and one TCP port carries HTTP, RTMP, and RTMPS
- [WebSocket caps](/quest/m3/rate-websocket.md) - paced reads and writes over bounded socket buffers hold WebSocket to the same caps
- [moq-webrtc pacing](/quest/m3/rtc-pacing.md) - a capped WHEP viewer never receives more than its cap plus burst, NACK resends included
- [SRT rate handle](/quest/m3/srt-rate-handle.md) - a per-connection cap paces SRT pulls, retransmits included, and closes SRT publishers past it
- [One port on the io_uring workers](/quest/m3/uring-demux.md) - `moq-uring`'s workers host the UDP demux, so a ring relay keeps WebRTC media and SRT on its QUIC port
- [Synced data playback](/quest/m3/watch-data-sync.md) - js/watch releases JSON and binary payloads on the media playhead, and a slow data track holds media back
- [WebCodecs audio priming](/quest/m3/webcodecs-priming.md) - `@moq/publish` measures each browser's `AudioEncoder` priming and corrects AAC timestamps and Opus pre-skip, so audio decodes at its capture timestamp
- [poll_acked on moq-net's send stream](/quest/m3/quic-ack-hook.md) - the
  backend-neutral hook on moq-net's own transport trait that awaits an
  acknowledged stream offset, implemented by the moq-tokio and moq-uring adapters
- [Relay io_uring packages](/quest/m3/relay-io-uring-package.md) - Linux relay packages ship io_uring once the ring is on par with tokio
- [Direct3D11 render import](/quest/m3/render-d3d11.md) - Windows presents without downloading every frame to system memory
- [Media Foundation encodes on the texture's adapter](/quest/m3/mf-encode-adapter.md) - Windows machines with two GPUs keep an exported texture on the GPU route into a hardware encoder
- [Intra-refresh GOPs](/quest/m3/intra-refresh/README.md) - video with periodic intra refresh imports and plays back cleanly with one group per sweep and a catalog `warmup`
- [#3115](/quest/m3/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink: a flushing restart after EOS opens a new publication generation
- [Realtime uplink congestion control](/quest/m3/quic-realtime/README.md) - native publishers pick `Realtime`, a receive-ts delay-based controller that keeps the uplink queue near empty for teleoperation
- [Windows capture parity](/quest/m3/capture-windows.md) - system and per-app audio and a settled app-capture policy
- [Linux capture parity](/quest/m3/capture-linux.md) - Wayland window/system-audio capture, per-app audio, and explicit display-selection and app-capture limits
- [Per-app audio on macOS](/quest/m3/capture-app-audio-macos.md) - one application's audio through ScreenCaptureKit
- [cpal loads libasound at runtime](/quest/m3/cpal-alsa-runtime.md) - condition: a cpal release whose Linux build carries no load-time libasound requirement
- [Audio capture without ALSA link](/quest/m3/capture-alsa-link.md) - moq-audio capture and playback build on Linux without linking libasound, and capture becomes a default feature
- [Ship capture and playback](/quest/m3/cli-packaging.md) - a released moq binary can capture and play, which no distribution currently enables
- [moq-relay in nixpkgs](/quest/m3/nixpkgs-relay.md) - `nix-shell -p moq-relay` installs the released relay from upstream nixpkgs, auto-updated from our tags
- [moq-cli in nixpkgs](/quest/m3/nixpkgs-cli.md) - the released `moq` CLI with its full release feature set joins nixpkgs as `moq-cli`
- [GPU capacity and health](/quest/m3/gpu-health.md) - moq-video reports each device's sessions, memory, utilization, and health on NVIDIA, AMD, and Intel alike
- [A release carries multi-vendor GPU input](/quest/m3/gpu-release.md) - `release` carries the GPU surface validated on AMD and Intel; health follows separately
- [The remaining library spawns](/quest/m3/spawn-audit.md) - every async spawn left in a library crate becomes a polled `kio::Tasks` child or is listed with why it stays
- [DVR rewind](/quest/m3/archive-dvr.md) - follow-on capability after the release contract is stable
- [Publisher timeliness at relay ingest](/quest/m3/qos-publisher-timeliness.md) - follow-on capability after the release contract is stable
- [The relay refuses a cluster peer it cannot dial](/quest/m3/relay-undialable-peers.md) - moq-tokio reports the schemes it can dial and names the missing feature when one can't, and the relay refuses an undialable peer at startup instead of retrying forever
- [moq-transport finish interop](/quest/m3/ietf-finish.md) - the interop `finish` case runs on every even moq-transport draft instead of being skipped
- [JS joining FETCH](/quest/m3/ietf-js-joining-fetch.md) - the JS publisher answers a joining FETCH that arrives after a quick finish, mirroring the Rust bound
- [An Absolute join never races an older-history FETCH](/quest/m3/ietf-absolute-join-race.md) - reproduce #7680's suspected collision on main, then fix it or keep the test and delete
- [A passing interop run stops publishers cleanly](/quest/m3/interop-shutdown-warnings.md) - no lingering publisher or unfinished-producer warning in a green interop run
- [Cost audit](/quest/m3/audit-cost.md) - CI jobs, tests, dependencies, build time, and artifact size each earn their cost, deleted by default
- [Code audit](/quest/m3/audit-code.md) - unused code is deleted, overbuilt code simplified, and every lint suppression still needed
- [Test coverage audit](/quest/m3/audit-test-coverage.md) - behavior that matters is tested and every test runs in CI
- [Documentation audit](/quest/m3/audit-docs.md) - docs match the code, with no stale pages, broken examples or links, or undocumented exports
- [Performance audit](/quest/m3/audit-perf.md) - benchmarks that regressed are explained, fixed, or planned, and `main` ships no slower than `release`
- [Interop audit](/quest/m3/audit-interop.md) - our interop runner entries match what we ship, and every red interop cell with a moq.dev side is fixed, reported, or noted as down or unsupported
- [Typecheck with bun check](/quest/m3/bun-check.md) - workspace packages typecheck through bun once nixpkgs ships bun 1.4.4
- [Clean ALSA playback onset](/quest/m3/audio-alsa-onset.md) - no onset restarts or clicks over PulseAudio
- [Read a bus source's own samples](/quest/m3/audio-strip-tap.md) - a tap reads one source's samples before its delay with their bus-clock positions, for the studio's clap sync
- [moq-audio filter chain](/quest/m3/audio-filters.md) - per-source suppression, gate, compressor, limiter, ducking, and plugins, which moq-dev/studio configures
- [moq-hls exposes each rendition's bound source](/quest/m3/hls-rendition-source.md) - playlists and segments report the source path and instance they read, so a cache can key on the source
- [The memory bench finishes at 1 MiB frames](/quest/m3/memory-bench-hang.md) - the allocation mode's park at `size=1048576` is root-caused and fixed
- [Audio's Latest start reaches back](/quest/m3/audio-latest-reach-back.md) - moq_audio's start covers min plus warmup like other consumers
- [moq-lite tails count drops](/quest/m3/lite-tail-drop-count.md) - SUBSCRIBE_SKIP and SUBSCRIBE_DROP accounting replaces the session-tail grace from lite-05
- [Audio track changes are gapless](/quest/m3/gapless-audio-switch.md) - no silence beyond a frame on audio format or rendition changes, enforced by media QA
- [We own our WebRTC stack](/quest/m3/webrtc/README.md) - one moq-webrtc crate holds the gateway, its ICE module, the data-channel transport, and a vendored, trimmed str0m/dimpl/is/sctp-proto
- [PUBLISH_DONE says why](/quest/m3/publish-done-codes.md) - GOING_AWAY, TOO_FAR_BEHIND, and EXPIRED round-trip as stream codes
- [A revoked announce resets instead of FINning](/quest/m3/announce-revoke-reset.md) - a PUBLISH_NAMESPACE revoked after its OK resets with INTERNAL_ERROR, per draft-22 §6.4.2.3
- [moq repair refuses a live recording](/quest/m3/mp4-repair-lock.md) - the recorder's advisory lock keeps `moq repair mp4` off a file still being written
- [The error-queue reader decodes truncated control data](/quest/m3/errqueue-truncation.md) - `MSG_ERRQUEUE` keeps what fit on `MSG_CTRUNC`, like the receive paths
- [The QUIC driver tells an MTU probe from an oversized send](/quest/m3/quic-emsgsize-classify.md) - a non-probe `EMSGSIZE` is logged and counted instead of dropped silently
- [fMP4 decode times from the declared reorder depth](/quest/m3/fmp4-declared-clock.md) - `DeclaredClock` replaces the sub-GOP cadence cap, as in TS, FLV, and MKV
- [An ended HLS playlist has no trailing gap](/quest/m3/hls-trailing-gap.md) - no zero-duration `EXT-X-GAP` before `EXT-X-ENDLIST`
- [/fetch answers from the announce sync](/quest/m3/fetch-synced.md) - a missing broadcast gets a 404 once announcements are synced, not after 30 s
- [An advertisement stuck on a zero-credit peer gives up](/quest/m3/advertise-credit-timeout.md) - A Rust moq-transport publisher whose PUBLISH_NAMESPACE or namespace update cannot leave because the peer grants no send credit gives up on it within a bounded time, as it already does on a silent answer: it resets the request and frees its state
- [moq-sock's control buffer is sized for the send options too](/quest/m3/cmsg-len-send.md) - `cmsg::LEN` in moq-sock is derived from, or checked at compile time against, both the receive options and the send options
- [A panic inside moq-tokio's connection internals fails callers instead of panicking them](/quest/m3/connection-panic-isolation.md) - after a moq-tokio QUIC connection driver panics, every client-side caller gets the connection's terminal error instead of panicking in its own task, and the certificate reload watcher and qmux driver tasks are supervised
- [No large future in the FFI layers can overflow a runtime or foreign thread's stack](/quest/m3/ffi-future-size.md) - every future moq-c and moq-ffi spawn or block on is below a size bound that a lint enforces, so unoptimized builds stay inside the runtime thread stack
- [An empty tail stream for a headless moq-transport group creates no group](/quest/m3/headless-empty-tail.md) - when a moq-transport subscription's fill for the Largest Group fails, an empty subgroup stream for that headless group is dropped instead of creating an empty group that reads as complete
- [A recording stalled on an open group reports what it waits for](/quest/m3/hls-stalled-segment.md) - when a moq-hls recording waits on a listed segment whose group a still connected publisher never closes, the wait is visible
- [A kept draft-18 join pins its head group](/quest/m3/ietf-join-pin-head.md) - on draft 18, a joining FETCH that our publisher keeps open after a quick finish is answered even if the publisher drops its `track::Producer` right after `finish()`
- [The JS player keeps a track's start when decoding from its first packet](/quest/m3/js-warmup-trim.md) - `@moq/watch` trims an audio track's catalog `warmup` only on a mid-stream join, as moq-audio does
- [A library recorder gets the takeover cut](/quest/m3/mux-source-cut.md) - A recorder built directly with `moq_hls::export::Broadcaster::new` ends a replaced instance's recording at what it listed when another publisher takes the broadcast over, exactly as one built through `Server::broadcaster` does
- [The OBS source joins audio at the live edge](/quest/m3/obs-decode-latest.md) - the OBS MoQ source's audio decoder starts at the newest cached group instead of replaying the track's backlog, so a reconnect or rendition change plays live audio at once
- [OBS simulcast layers reach moq-watch from a live OBS, including the dock's Go Live](/quest/m3/obs-multitrack-check.md) - A live OBS 32.2.2 run, recorded in the PR, shows the `WHIPSimulcastTotalLayers` profile setting attaching two or more scaled video encoders to `moq_output`, each published as its own rendition and watched in `<moq-watch>`, both through the obs-websocket recipe in `doc/bin/obs.md` and through the MoQ dock's Go Live, which honors the profile's layer count instead of creating a single encoder
- [Opus export never writes a channel mapping it can't stand behind](/quest/m3/opus-surround-export.md) - exporting a catalog Opus rendition with 3 to 8 channels and no description writes a `dOps` with the Vorbis family 1 mapping table, as the TS importer assumes, instead of an invalid family 0 `dOps`
- [Open-GOP MPEG-2 export trims leading B-pictures in zero-stuffed CBR streams](/quest/m3/pes-hold-open-gop.md) - A TS export starting or resuming inside an open GOP of MPEG-2 video drops the orphaned leading B-pictures even when their PES ends on zero stuffing, as CBR encoders emit, without ever trimming part of a reference picture whose start code straddles two PES packets
- [A multi-program TS import survives a source switch](/quest/m3/ts-multi-program-restart.md) - `--program all` restarts each affected program on an unannounced source switch, as a single-program import does after moq-dev/moq#7780, instead of ending the whole import
- [The lite publisher tells transport faults from cancels](/quest/m3/publish-cancel-classify.md) - `rs/moq-net`'s lite publisher no longer counts every `Error::Transport` as a clean cancel
- [tls:// sessions report their negotiated ALPN](/quest/m3/tls-link-alpn.md) - A session accepted on a `tls://` listener reports the negotiated moq ALPN in `Link::alpn`, as raw QUIC, WebTransport, and WebSocket sessions already do, so admission and logs see the protocol on every transport
- [PUBLISH token renewal is consistent across pause and forward, and the multi-token cost is profiled](/quest/m3/token-auth-followups.md) - on a moq-transport session, a REQUEST_UPDATE carrying an AUTHORIZATION TOKEN on a PUBLISH gets the same answer whether the PUBLISH is paused or forwarding
- [An audio-only broadcast that can't play hides its play button](/quest/m3/audio-only-play.md) - center-play follows the audio-only rule, proven by a browser test
- [moq-quic's long header type is one variant](/quest/m3/quic-long-type.md) - collapse LongType after the 0-RTT removal
- [One predicate for the lite close wait](/quest/m3/subscriber-fin-fold.md) - fold waits_for_subscriber_fin into has_track_stream
- [MKV imports ffmpeg's multichannel FLAC](/quest/m3/mkv-flac-channel-mask.md) - a channel-mask-only Vorbis comment is accepted, so default ffmpeg multichannel FLAC needs no remux
- [The publish support panel lists only codecs publishing accepts](/quest/m3/publish-support-probe.md) - the settings panel runs the publisher's Annex B encoder probe
- [TLS reload follows symlinked certificate files](/quest/m3/watch-symlink-targets.md) - in-place rewrites of symlink targets in other directories reload roots, served chains, and client certificates
- [A published capture exposes its bandwidth grant](/quest/m3/video-publish-grant.md) - a moq-video publish shows each track's granted bitrate, not only its target
- [The test relay can enable stats](/quest/m3/relay-test-stats.md) - `moq_relay::test_relay` starts with stats on when asked
- [macOS capture listing futures are Send](/quest/m3/capture-shareable-send.md) - ScreenCaptureKit listing and start no longer hold a non-Send block across an await
- [A capture listing a picker can draw](/quest/m3/capture-list.md) - windows grouped by app with icons and placed on their displays, camera kinds, low-rate pictures, and a wait for changes on macOS, Windows, and X11, for moq-dev/studio's Add page
- [Denied macOS system audio names its settings pane](/quest/m3/audio-system-denied-hint.md) - a permission error pointing at Screen & System Audio Recording
- [One encoder follows several connections' estimates](/quest/m3/bandwidth-multi-connection.md) - a track published to two relays tracks both estimates
- [Bandwidth reservations ignore in-process readers](/quest/m3/reservation-local-readers.md) - a local recorder no longer counts as uplink demand
- [Container export starts apart from its skip budget](/quest/m3/export-live-start.md) - fMP4 and MP4 export start at the live edge and report sample counts
- [MP4 export trims exactly at both ends](/quest/m3/mp4-export-window.md) - a presentation window with no lead-in and no presented tail
- [Container export ends cleanly when its broadcast closes](/quest/m3/export-close.md) - every container export returns `None` when its instance closes, whichever signal arrives first
- [Audio capture opens a format the mix accepts](/quest/m3/audio-capture-negotiate.md) - multichannel and odd-rate devices capture, with discrete input selection
- [moq-mux imports a regular MP4](/quest/m3/mp4-import.md) - non-fragmented MP4, `moov` first or last
- [An audio encoder flushes without ending its track](/quest/m3/audio-encode-flush.md) - a clip's last partial frame is heard while the source stays live
- [Padding probes from the browser](/quest/m3/probe-padding.md) - parked: the maintainer doubts padding is the right way for a browser to probe
- [JS rung disable](/quest/m3/probe-rung-disable-js.md) - `@moq/publish` applies the native rung rule, recovering through padding; parked with it
- [Layered broadcasts](/quest/m3/layers/README.md) - hang specifies a layer layout track, and `moq play` and `@moq/watch` composite layers over the main picture
- [moq-rtmp dials rtmps:// and always advertises a tcUrl](/quest/m3/rtmp-rtmps-tcurl.md) - the RTMP client dials an `rtmp://` or `rtmps://` URL and sends its `tcUrl` on both, for moq-dev/studio's destinations
- [moq-webrtc and moq-rtmp dial through a caller's policy](/quest/m3/webrtc-dial-policy.md) - a host that dials untrusted URLs refuses private, loopback, and link-local addresses on every WHIP, WHEP, and RTMP dial
- [A room keeps a participant while one of its aliases is announced](/quest/m3/room-alias-withdraw.md) - withdrawing `camera` while `camera.hang` stays announced keeps the participant
- [@moq/auth key derivation and key sets validate like single keys](/quest/m3/auth-key-validation.md) - `Key.public` shares no scope and never yields an invalid key, and key sets and Rust refuse mismatched keys at parse
- [Binding server helpers observe session errors and describe close truthfully](/quest/m3/serve-session-errors.md) - Python retrieves session task errors, and the Kotlin, Go, and Dart docs say close ends every connection
- [A GOAWAY to a draining fallback session is followed](/quest/m3/reconnect-probe-redirect.md) - a redirect that reaches a draining fallback session is not lost
- [A connection reports Migrating while its predecessor still serves](/quest/m3/reconnect-predecessor-status.md) - Disconnected means nothing serves
- [A group read to its FIN stays finished across updates](/quest/m3/lite-finished-across-updates.md) - a lite-07 subscriber never records a group it got whole as stale
- [A dead redirect target falls back to the configured relay](/quest/m3/net-redirect-fallback.md) - `@moq/net` stops redialing a dead redirect target
- [Room participants survive your own connection blip](/quest/m3/room-keep-through-blip.md) - remote participants stay on screen across the local user's reconnect
