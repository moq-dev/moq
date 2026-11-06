# m4: later work, lower priority

## Goal

Work that follows the next breaking release with nobody waiting on it yet:
tests, tooling, benchmarks, optimizations, conditions on outside parties that
only later work needs, and features only a downstream deployment's later
plans need.

## Plan

Decided 2026-11-05 (maintainer, milestone re-cut): the lower half of the old
m2 ("later work"), as [m3](/quest/m3/README.md) describes. After each cut
m4's quests move to m3 and m4 starts empty. A quest here that gains a
consumer moves to m3; one that loses its last reason moves to m5 or is
deleted.

## Required

- [Wire fuzz coverage](/quest/m4/wire-fuzz-coverage.md) - the cmsg walks in moq-sock and moq-uring have invariant fuzz targets, and the unsafe peer input reaches runs under Miri
- [VA-API renderer tests](/quest/m4/vaapi-render-tests.md) - the VA-API renderer, decode, and transcode tests run in `just rs gpu` on AMD and Intel and fail instead of skipping, and hosted CI reports them ignored
- [One Ruff pin and Swift formatting on aarch64-linux](/quest/m4/format-gate-followups.md) - only the dev shell pins Ruff, and aarch64-linux checks Swift formatting once nixpkgs builds `swiftc` there
- [Weekly mutation testing on critical modules](/quest/m4/mutants.md) - cargo-mutants runs weekly on moq-auth and moq-net's permission, resume, and expiry code
- [P2P](/quest/m4/p2p/README.md) - application-selected peer sessions use shared priorities, routing, and same-epoch handover
- [moq-auth drops the unused peer grant API](/quest/m4/auth-drop-peer-grant.md) - P2P trusts its peers, so `PeerGrant` and its `KeySet` methods go
- [Benchmark sources](/quest/m4/bench-sources.md) - every measuring nightly benchmark feeds the regression table, and none is a smoke run
- [HLS and DASH browser playback](/quest/m4/hls-player-test.md) - hls.js and dash.js play, reload, switch, and survive reconfiguration nightly
- [Cache fair share](/quest/m4/cache-fairness.md) - one tenant cannot crowd another out of the relay's shared group cache
- [Peer grant bound](/quest/m4/peer-grant-bound.md) - a peer's AUTH_OK costs near-linear time in its bytes on every input shape, in Rust and in a browser
- [A regrant re-checks only the routes it can change](/quest/m4/regrant-route-recheck.md) - an `AUTH_OK` no longer costs routes times grant members on every lite announce stream
- [Upstream reports](/quest/m4/upstream-reports.md) - report the faad, retina, and mr-boxington bugs found before the cut
- [moq-net's library tests pass under cargo test](/quest/m4/net-test-subscriber.md) - two moq-net tests stop each installing a global tracing subscriber, so plain `cargo test -p moq-net --lib` passes
- [Perf](/quest/m4/perf/README.md) - eliminate measured hot-path costs across moq-uring, kio, and the moq-net model
- [Transport x version benches](/quest/m4/bench-transport-sweep.md) - the session benches sweep transports as well as versions
- [Shared group serving and ingest](/quest/m4/shared-serve.md) - Rust's moq-lite and moq-transport sessions serve and ingest groups through one poll machine each, with only wire encoding per protocol
- [Shared group serving and ingest in JS](/quest/m4/shared-serve-js.md) - the same split in js/net, following the Rust seam
- [Paired bench BASE](/quest/m4/bench-base-paired.md) - `just bench <base>` uses the nightly's paired comparison and the old one is deleted
- [Route wakes](/quest/m4/route-wakes.md) - a route change wakes only the fronts it can move, so pool churn stops scaling with served paths
- [Audio outlives video](/quest/m4/audio-bufferbloat-av.md) - under bufferbloat audio stays within budget while video may skip, rendered frames stay in sync, and native audio rows are enforced
- [ViSQOL grading](/quest/m4/audio-visqol.md) - the nightly scores played audio against a reference clip and fails below measured floors
- [Browser publishing paths under test](/quest/m4/publish-firefox.md) - the media harness publishes from Firefox in Nightly and passes, covering the `<video>` capture fallback Chromium never takes
- [Bindings in the interop matrix](/quest/m4/interop-bindings.md) - Kotlin, Swift, and Dart publish and subscribe against every other interop client
- [Resume as producer replacement](/quest/m4/resume-redesign.md) - route changes replace the writer of one track state, deleting the per-reader resume path in Rust and JS
- [Announce interest streams share one route table](/quest/m4/announce-interest-memory.md) - per-stream announce cost follows what is in flight, not every route
- [Watch decode gate](/quest/m4/watch-decode-gate.md) - video lookahead stays encoded until it is near presentation, like audio
- [moqsrc A/V sync](/quest/m4/moqsrc-av-sync.md) - a nightly test measures A/V skew from `moqsrc` through clock-synced sinks
- [macOS browser lane](/quest/m4/macos-browser-lane.md) - nightly CI runs the priming browser test in Chrome and Safari, which have the AAC encoders Linux lacks
- [fMP4 ambisonic layout](/quest/m4/mux-vr-ambisonic-fmp4.md) - fMP4 Opus import fills `ambisonic` from the `dOps` mapping table
- [io_uring quiescence on a quiet host](/quest/m4/uring-bench-followups.md) - quiet-host acceptance numbers and a nightly `worker_turns` QUIC request-reply series
- [#2819](/quest/m4/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - moq-video: validate PipeWire DMA-BUFs into the Vulkan renderer on hardware
- [Windows.Graphics.Capture](/quest/m4/capture-wgc.md) - the WGC display and window backend verified on real Windows hardware
- [The moq CLI can publish a changing catalog](/quest/m4/cli-catalog-updates.md) - catalog updates mid-stream, so recovery replays need no hand-built clients
- [The relay over QUIC on the sim](/quest/m4/quic-sim.md) - the relay and moq-tokio's `Client` and `Server` run over `moq-quic` on the seeded sim
- [moq-shaper's own socket never passes for path loss](/quest/m4/shaper-socket.md) - relay-sized buffers and its own socket drops reported beside path drops
- [Auth session caps cost O(1)](/quest/m4/auth-session-index.md) - per-token and per-remote counts and a time-ordered expiry queue replace the scans, after #7211
- [Drop moq-auth's iroh transport](/quest/m4/auth-drop-iroh.md) - the next breaking moq-auth release drops `Transport::Iroh` and the JS `"iroh"` value
- [Relays and moq-tokio offer versioned qmux-02 first](/quest/m4/qmux-02-subprotocols.md) - one shared qmux version list prefers 02, so JS clients negotiate it
- [Smoke tests make their certificates without a system OpenSSL config](/quest/m4/smoke-libressl.md) - moxygen and Cloudflare smoke pass `-config`, as the conformance runner does
- [ashpd uses maintained D-Bus crates](/quest/m4/ashpd-maintained.md) - upgrade to ashpd 0.14 (zbus 6) and remove the portal dependency exceptions
- [Askama uses a maintained TOML parser](/quest/m4/askama-maintained.md) - wait for a stable parser migration, then remove the generator dependency exception
- [Adopt formatter defaults after evaluating Bun](/quest/m4/formatter-defaults.md) - every language uses its formatter's defaults, YAML and Markdown are formatted, and remark is gone in one coordinated migration
- [Upstream unknown-env option in usage-rs](/quest/m4/usage-unknown-env.md) - jdx/usage refuses unknown prefixed env vars, replacing the moq-tokio helper
- [Upstream single-declaration settings in usage-rs](/quest/m4/usage-settings-once.md) - waiting on approval to post, then jdx/usage builds the registry from Cli fields
- [Archive reads against a real store](/quest/m4/archive-store-bench.md) - LIST/GET counts and latency over track count
- [Branch checks lint examples](/quest/m4/lint-examples.md) - `just check` runs clippy on the selected crates' examples, and PR CI builds the C++, Go, and Python interop clients
- [Drop kio's epoch fence](/quest/m4/crossbeam-pin-fence.md) - waiting on crossbeam#1352, then the `Read::drop` fence goes
- [A cancelled miss stops waiting](/quest/m4/fill-wait-cancel-test.md) - a test proves a relay miss waiting on its own fill ends when no reader wants it
- [No MOQ_* knobs outside the binaries](/quest/m4/env-knobs.md) - test, bench, and library knobs leave the `MOQ_*` prefix
- [The HLS server follows each path once](/quest/m4/hls-follow-once.md) - `Following` reports synced absence, so moq-hls resolves with one follow
- [moq-bench pins the instances it measures](/quest/m4/bench-epoch-pin.md) - pooled requests pin their announced instance, so another instance at the path is not counted as the same broadcast
- [moq-bench reports sub-millisecond latency](/quest/m4/bench-latency-resolution.md) - steady-window latency resolves finer than 1 ms
- [Benchmark series stay meaningful](/quest/m4/bench-hygiene.md) - a warm `burst` variant, stale series pruned, and slow-group `bytes` kept out of series names
- [Re-run the SPS fuzzer on h264-reader](/quest/m4/sps-fuzz-h264-reader.md) - the H.264 SPS overflow is rechecked on the new parser
- [Drop the H.265 SPS pre-walk once scuffle is fixed](/quest/m4/scuffle-sps-bump.md) - waiting on ScuffleCloud/scuffle#653 and #654 releases, then bump and drop the walk
- [PCM encodes in one pass](/quest/m4/pcm-bulk-encode.md) - the PCM encoder converts a frame in bulk instead of one sample at a time
- [moq-quic's internal dev-dependencies are path-only](/quest/m4/quic-dev-dep-path.md) - moq-congestion is a path dev-dependency
- [moq-mux's AAC config reports SBR](/quest/m4/aac-sbr-config.md) - moq-audio reads backward-compatible SBR from the config
- [moq-video's Apple Sink encodes on its own thread](/quest/m4/apple-sink-thread.md) - macOS and iOS ladders never encode on a tokio worker
- [aec::Processor's edge cases are tested](/quest/m4/aec-tests.md) - moq-audio tests cover `aec::Processor` at the extremes of its accepted input
- [A browser test covers GPU encoder death and a broadcast rename](/quest/m4/gpu-encoder-death-test.md) - A real-browser test makes `@moq/publish`'s video encoder fail mid-stream
- [kio::Tasks tolerates a removed task's waker firing into its reused slot](/quest/m4/kio-tasks-waker-test.md) - A unit test in `rs/kio/src/task.rs` shows that when a task taken out with `Tasks::remove` wakes after another task was `insert`ed into its slot, the new occupant is polled at most once spuriously, still finishes on its own wake (handed back through `poll_each`), and its `TaskKey` still removes it
- [moqsrc reconnects correctly whatever order events arrive in](/quest/m4/moqsrc-reconnect-order.md) - A moq-gst test reconnects `moqsrc` while a rendition pad is blocked downstream, delivering the reconnect's events
- [The relay NixOS module's peer list is tested](/quest/m4/nix-relay-peers-test.md) - `nix flake check` evaluates the moq-relay NixOS module with no, one, and several `cluster.rootUrl` peers and refuses an empty list, asserting the `MOQ_CLUSTER_CONNECT` value each produces
- [moq-quic's sim pair fails a test that sends an oversized packet](/quest/m4/quic-sim-mtu.md) - A moq-quic test whose endpoint sends a datagram larger than the simulated path MTU fails, instead of the `Pair` silently dropping it
- [A stalled reader within budget gets no Old resets through a relay](/quest/m4/relay-capped-credit-test.md) - A test proves that a subscriber which stalls before its first read, behind a relay over a transport with capped stream credit, receives no `Old` stream resets for groups still within its max-delay budget, and reads them in order once it resumes
- [In-memory transports refuse a request without stream credit](/quest/m4/sim-credit-open.md) - moq-net's in-memory test transports model the peer's bidirectional stream credit, and `poll_open_request` returns `Ok(None)`
- [The tenant matrix's HTTP checks assert exact refusals](/quest/m4/tenant-leak-checks.md) - the relay tenant matrix's HTTP check asserts a definite outcome for each cross-tenant request, so it cannot pass by timing out or by an unexpected status
- [The test inventory follows every branch selection](/quest/m4/test-inventory-followups.md) - `sh/gh/test-inventory.py` judges every check by the dispatcher branch its caller actually selects, including the cargo-fuzz caller check, the recipe-runs-tests check
- [moq-uring's handle lifecycle tests cover the newly spawned queue and a woken close watcher](/quest/m4/uring-lifecycle-tests.md) - two regressions cover the shutdown paths moq-dev/moq#7309 added
- [Watch tests wait for the change they expect](/quest/m4/watch-test-waits.md) - `js/watch` tests wait for the signal or state they assert on instead of a fixed number of event-loop yields, so they neither flake under load nor pass before the change happens
- [JS eviction tests prove the eviction happened](/quest/m4/track-test-eviction-asserts.md) - `track.test.ts`'s eviction tests fail with eviction off
- [Nightly runs zizmor's online workflow audits](/quest/m4/workflow-audit-followups.md) - nightly runs zizmor's online audits
- [Browsers expose WebTransport receive credit](/quest/m4/browser-receive-credit.md) - condition: the page can tell how much credit an unread stream holds
- [JS fastest-reader pacing](/quest/m4/js-fastest-reader.md) - `@moq/net` paces upstream groups by the fastest reader, once browsers allow it
- [fMP4 import at 1-byte chunks is no slower than before](/quest/m4/fmp4-byte-chunk-perf.md) - bisect and fix or accept the ~19% slowdown at 1-byte chunks
- [The relay bench reports io_uring pool size and RSS](/quest/m4/bench-uring-pool.md) - pool growth regressions show up in normal bench runs
- [The bindings' TrackInfo docs list its real fields](/quest/m4/track-info-docs.md) - Swift, Kotlin and Dart stop naming ordering and latency budget on `TrackInfo`
- [Kotlin dependencies live in a Gradle version catalog](/quest/m4/kt-version-catalog.md) - one catalog entry per library, so a Dependabot bump can't drag another along
- [The dev shell builds Kotlin with Gradle 9](/quest/m4/kt-gradle-9.md) - `gradle_9` in the dev shell, so the Android target builds there
- [A hung Python test fails instead of hanging the suite](/quest/m4/py-test-timeouts.md) - one pytest-timeout bound covers every `py/` test
- [Chromium's Opus encoder stamps chunks from their input](/quest/m4/chromium-opus-timestamps.md) - waiting on Chromium; unlocks native `usedtx` for push-to-talk
