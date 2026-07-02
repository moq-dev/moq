# Dev branch review (`origin/dev` vs `origin/main`)

Date: 2026-07-01. Scope: the full 136-commit, 592-file diff. Method: `just check` on dev (green, 396 moq-net tests + 308 moq-mux tests + all JS suites pass), plus per-subsystem deep reviews of wire parity, API shape, and hookup. Findings that also apply to `main` were filed as GitHub issues [#1999](https://github.com/moq-dev/moq/issues/1999)–[#2010](https://github.com/moq-dev/moq/issues/2010) (table at the bottom); everything below is dev-only unless noted.

**Still pending:** the Go-binding sub-review and two aggregator roll-ups. This file will be updated if they surface anything new.

---

## Verdict in one paragraph

The build is green and the API reshapes are largely well-executed (input/output signals, two-phase `TrackRequest.accept`, `OriginPublish` RAII guards, the gateway `Config` + `run` + `Server`/`Request` pattern, `Timescale(NonZero)`). But there are several runtime-only breakages a green build can't catch: MoQ Boy playback is fully broken, the JS group cache kills any group over 32 MB, wasm clients panic against lite-04 relays, relay subscription merging corrupts multi-viewer state, and moq-rtc/moq-hls have "won't work by default" defects. Two areas of dev (moq-mux, the HLS importer, js qmux pin) are now **stale relative to main** and need a main→dev merge before further work.

---

## Must-do before dev merges to main

1. **Revert the lite-05-wip default ALPN.** `rs/moq-net/src/version.rs:23-33` (`ALPNS[0]`, `Versions::all()`) and `js/net/src/connect.ts:360` both prefer `moq-lite-05-wip`, each carrying a "revisit before promoting to main" comment. Known temporary measure; shipping it makes every default binary prefer a still-moving wire format.
2. **Decide the quinn escape hatch for the noq default flip (#1891).** `quinn` was dropped from default features in moq-native/relay/cli (and everything downstream: ffi, libmoq, gst, all language bindings). `QuicBackend::Quinn` is `#[cfg(feature = "quinn")]`, so a default build can't even parse `--server-backend quinn`; rolling back from a noq regression requires a recompile. Recommend release binaries compile both, noq as default.
3. **Merge main into dev.** Dev's moq-mux is missing main's #1925/#1933/#1941 (API cleanup), `select::Broadcast`, Opus-over-TS, MP3, FLAC, and the `pub use mp4_atom` re-export; dev still ships the deleted `catalog::Filter`/`Target` shapes. Dev's HLS importer lacks main's sequence-reset re-anchor and discontinuity handling. Dev's `js/net` still pins qmux 0.1.1 (missing the WebSocket-hang fix from PR #1957).

---

## Critical

### C1. js/net: group cache accounting breaks every group after 32 MB cumulative (reproduced)
`js/net/src/group.ts:87-98` increments `#cacheBytes` on every `writeFrame` but never decrements on read, so it tracks cumulative bytes written, not bytes buffered. Once a group passes `MAX_GROUP_CACHE_BYTES` cumulative, every subsequent write evicts the just-written frame and all `readFrame()` calls throw `CacheFull`, even for a caught-up reader. Reproduced with 40x1 MB frames read immediately: throws at frame ~33. Any high-bitrate GoP, screen share, or long-lived data group breaks mid-group for every subscriber. Fix: decrement in the read paths or compute the cap over the buffered array only.

### C2. js/moq-boy: playback fully broken (no `supported` probe wired)
`js/moq-boy/src/game.ts:124,128` builds `Watch.Video.Source` / `Watch.Audio.Source` without the new `supported` input; the old self-registration in the decoders was removed. `Source.#runSupported` returns early, `available` stays empty, no track is ever selected: every tile black and silent forever. One-line fix per source (`supported: Watch.Video.Decoder.supported` etc.). Consider defaulting `supported` in `Inputs` so manual Source+Decoder assembly can't hit this cliff.

### C3. moq-net: `Timestamp::now()` panics on wasm, now on the receive hot path
`rs/moq-net/src/model/time.rs:313,420-426` uses `std::time::Instant::now()` + `SystemTime::now()`, both of which panic on `wasm32-unknown-unknown`, and mandatory timestamps made `create_frame_now` run for every frame received on a pre-lite-05 session (`lite/subscriber.rs:625`). A `@moq/wasm` browser client subscribing via any lite-04 relay (anything built from current main) panics on the first frame. Works in the dev demo only because both ends default lite-05. Fix: route through `web_async::time` as the crate already does elsewhere.

---

## Major

### moq-net (`rs/moq-net`)

- **`Subscription::poll_combined` merges viewer preferences incorrectly** (`model/subscription.rs:76-82`): `ordered` is NAND'd (two ordered viewers produce `ordered=false`, flipping with viewer-count parity); `group_start: min(Option)` treats `None` as negative infinity so a "start at latest" viewer erases another's explicit start; `group_end: max(Option)` caps the aggregate at a bounded viewer's end. Concrete failure: viewer A live (`group_end=None`) + viewer B bounded catch-up (`Some(10)`) on the same relay track caps the upstream SUBSCRIBE at group 10, freezing A's stream while B is connected. No test coverage.
- **IETF drafts 14/15 object-property KVPs use delta-typed encoding** (`ietf/group.rs:22-85`) where those drafts define absolute types. The crate's own `Parameters` impl gets the cutoff right (absolute on 14/15, delta on 16+). Against a compliant peer (moxygen), the Timescale property is skipped and timestamps get interpreted 1000x too small. Self-interop hides it.
- **A `GroupRequest` dropped without `accept()` strands `fetch_group` waiters forever** (`model/track.rs:1151-1178`): no `reject()`, no Drop side effect, unlike `BroadcastRequest`/`TrackRequest`. Deterministically hit when serving a fetch on a Lite01/02 session (`serve_fetch` returns before accept on `EncodeError::Version`) and on transient `open_uni` failure. Fix: add `reject()` + drop-rejects, and version-gate fetch serving.

### js/net

- **lite FETCH is one-sided**: JS neither serves (`#runBidi` has no `StreamId.Fetch` case → `unknown stream type: 3` reset against a Rust lite-05 subscriber) nor sends FETCH, while Rust lite-05 relies on it for group re-fetch. Implement or explicitly scope out.
- Minor wire notes: explicit ANNOUNCE `restart` status byte 2 (legal per draft) kills the JS announce stream (`r.bool()` throws); stream-type prefixes read as `u8` where Rust decodes varint (breaks only for future types ≥ 0x40); JS accepts negative accumulated frame timestamps Rust rejects; exact-match `=== DRAFT_05_WIP` gates in announce paths default backward instead of forward (both languages; a Lite06 bump silently reverts announce behavior).

### moq-video / moq-audio (all dev-only; Windows paths compile-checked only, never run)

- **Windows DXVA decode: NV12 UV-plane offset assumes texture height == display height** (`decode/backend/mediafoundation.rs:193`, `frame.rs:504`). DXVA pool textures are coded-size (1088 for 1080p), the display aperture is never consulted: garbage chroma or 8 junk rows at the most common resolution. Fix before any Windows decode ships.
- **VideoToolbox encode treats `CMBlockBufferGetDataPointer` output as contiguous without checking** (`encode/backend/videotoolbox.rs:263-274`): OOB read if non-contiguous. Guard `length_at_offset == total`.
- **COM init/uninit thread mismatch**: since capture went async (#1807), MF encoder/decoder construction, use, and drop can land on different tokio workers; `ComGuard` unbalances per-thread COM refcounts. The `unsafe impl Send` safety comments cite a `spawn_blocking` that no longer exists.
- **`wait_for_input` blocks a tokio worker indefinitely** (`encode/backend/mediafoundation.rs:327-337`) on a stalled MFT: resurrects the Ctrl+C-hang class #1807 fixed, on Windows.
- **Audio permission prompt blocks a runtime worker up to 30s** (`moq-audio/src/capture/permission.rs:50-75`): `recv_timeout` on the now-async `Microphone::open` path. The video side bridges the same callback correctly; mirror it.
- #1807 also made `publish_capture`/`publish_microphone` futures `!Send` (silent capability regression for external `tokio::spawn` callers) and left a narrow cancellation race that can strand the capture pump thread (camera LED stays on).

### FFI + language bindings

- **Release-coordination trap on the moq-ffi version**: `py/moq-rs` 0.3.1 pins `moq-ffi ~= 0.2.24` and Kotlin pins `[0.2,0.3)`, while both wrappers use symbols (`MoqTrackRequest`, `MoqSubscription`, `announce`, async `subscribe_track`) that no publishable 0.2.x has. Ship the bindings as 0.2.25 and every previously published wheel floats onto them and breaks; ship 0.3.0 and the new wrappers resolve stale bindings and die at import. Decide the bump + pin strategy before any release off dev.
- **Swift/Kotlin lib release workflows publish on pushes to dev**, and the Swift cross-package `verify` job is skipped (not failed) when the pinned moq-ffi isn't mirrored yet; Maven Central artifacts are immutable. A dev push can tag a public release no consumer can resolve.
- **Swift `moqStream` uses an eager unbounded `AsyncThrowingStream`**: no backpressure, defeats the `maxLatencyMs` jitter-buffer GoP-skipping; a slow consumer grows memory indefinitely instead of skipping forward. Affects every `for try await` in the package. Use pull-per-demand or `bufferingNewest`.
- **Python `TrackRequest.accept()` hardcodes `accept(None)`**, dropping the FFI's `TrackInfo` parameter (timescale/priority/cache unsettable on dynamically requested tracks).
- **Wrapper surface holes**: `Session.stats()`/`MoqConnectionStats` unreachable from Python and unwrapped in Swift; Swift also missing dynamic tracks, client TLS pinning, `Announcement.hops()`, `TrackProducer.abort`.
- **Doc examples that throw as written**: `doc/lib/py/moq-rs.md` (stale `moq.MoqError`, missing `await` on now-async `subscribe_media`/`subscribe_track`, dynamic-track example calls methods on `TrackRequest` that don't exist), `py/moq-rs/README.md` quickstart (`async with` on a coroutine), `doc/lib/kt/moq.md` and `doc/lib/swift/moq.md` on-demand-track examples (compile against neither wrapper nor FFI; `origin.publish` vs `announce`).
- Naming drift: rs/moq-ffi renamed `publish` → `announce` to avoid the `publisher().publish()` stutter; Python kept `publish`, Kotlin inherits `announce`. Pick one.
- `doc/lib/c/index.md`'s callback `user_data` lifetime list omits the new `moq_consume_video_raw` (the only place the contract is documented).

### Gateways (dev-only findings; main-shared ones are in the issue table)

- **moq-gst: async `reconcile` can wedge the session loop** (`src/source/imp.rs:343,430`): per-rendition `subscribe(None).await` resolves only when track info arrives; a catalog listing a track that's never accepted parks the await, the select loop stops polling shutdown, and `moqsrc` `stop()` blocks the GStreamer state change indefinitely. Race reconcile/subscribes against shutdown.
- **moq-srt: egress mux latency budget hardcoded to zero** (`src/ts.rs:82`, no `.with_latency`): routine jitter at group boundaries triggers mid-GoP truncation (decode artifacts) with no config knob. Also: forward-only re-anchoring permanently degrades SRT TSBPD pacing after a backward timestamp jump; egress disconnect detected only via `send` so a stalled broadcast leaks viewer tasks; README auth example doesn't compile (`tokio::spawn` on a borrowing future) and mentions a `Request::reject()` that doesn't exist; no `doc/bin/srt.md` while every sibling gateway has a page.
- **moq-rtc API shape**: `session`/`egress`/`ingest`/`codec` modules are all `pub` and leak `str0m::Rtc`, `Mid`/`Pt`, tokio channels, and `bytes` across ~20 signatures; a str0m 0.20 bump becomes a breaking change for downstreams. Shrink to `Client` + `Server` + `whip::/whep::accept` + `Response`. Also `server::Config`/`client::Config` lack `#[non_exhaustive]`; `whip::accept`/`whep::accept` take 4 positional args (redundant `server` + origin); WHEP accept awaits the first catalog with no timeout inside the HTTP handler.
- **moq-hls (dev-only parts)**: resume after a short pause emits `EXT-X-DISCONTINUITY` on a segment starting mid-GoP with `independent=false` (decoder reset onto P-frames: green video) whenever the pause was shorter than the relay cache; pause is embed-only API (no route or CLI flag reaches `set_paused`) and only takes effect at the next fragment; playlist advertises `EXT-X-MAP` before the init segment exists (404 window); no `Cache-Control` headers on any response (CDN deployments will misbehave); blocking-reload skips the spec's `_HLS_msn`-too-far 400; rendition names collide across the video/audio axes (one `BTreeMap` for both); detached pump tasks outlive the server; export `Config`s lack `#[non_exhaustive]`, `import::Config.client` leaks `reqwest::Client`, `SegmentStore::new(bool, f64, f64, f64)` and 5-positional-arg `Rendition::video/audio` violate the misuse-resistance rules; **no auth hook anywhere** (any HTTP client can pull any broadcast the origin sees; consider a `Request`-style seam like moq-srt/rtc).

### Relay / native / CLI

- `doc/bin/cli.md` subscribe surface incomplete: `--format h264|h265` and the nine new `--video-*`/`--audio-*` rendition flags documented nowhere.
- Capture build example (`doc/bin/cli.md:117`, `rs/moq-cli/Cargo.toml:31-33`) still says `--features "iroh quinn websocket capture"`, silently building a quinn binary post-flip.
- `publish hls` kept but not hidden (`hide = true` missing) contra the deprecation policy; same for the `#[deprecated]`-but-documented `with_publish`/`with_consume` aliases (need `#[doc(hidden)]`).
- False `--help` claim: `rs/moq-cli/src/main.rs:87` "path currently must be `/`" contradicts every documented example.
- `rs/moq-relay/src/cluster.rs:597,24,1005`: bad search-and-replace ("restartments", "restart flap") where "re-announce" was meant.
- `just --fmt` mangled recipe doc comments in the root and `demo/pub` justfiles (severed sentences in `just --list`, broken `\` continuation in the ffmpeg example).

### JS media / catalog

- **Browser publisher's catalog now emits merge-patch deltas by default** (`js/publish/src/catalog.ts:31` omits `deltaRatio: 0`; @moq/json defaults to 8). The deleted hang producer pinned deltas off deliberately for snapshot-only consumers. First-party consumers reconstruct deltas so demos work, but this is a silent wire-behavior change: confirm it's the intended delta rollout or pin `deltaRatio: 0`.
- Demo regressions: `meta.json` subscription priority dropped 100 → 0 (can starve behind media on congestion); publish demo leaks a module-level Effect on every HMR reload.
- `hangz` is JS-only on dev (Rust #1904 landed on main post-merge-base); self-resolves at the main→dev merge.

---

## Notable minors (condensed)

- moq-net: `has_timestamps` doc is stale and the predicate gates four unrelated lite-05 features (rename to `has_track_stream` or similar); publisher announce initial-drain can announce a stale restarted entry (narrow cluster race); `Origin { pub id: u64 }` with id 0 is representable but torn down by lite-05 peers at decode (validate at construction); `SubscribeEnd` on a group-less track claims bound 0; `Request::path` UTF-8 validation differs between lite (reject) and IETF (lossy); `get_group` on an evicted sequence parks until track close (doc gap steering to `fetch_group`).
- moq-net API: `TrackInfo`, `Subscription`, `Fetch`, `BroadcastInfo` should get `#[non_exhaustive]` now (pub-field structs with builders, certain to gain fields; purely additive today, breaking later).
- moq-mux (dev copy): FLV HEVC path (composition-time offsets) has zero tests, FLV AV1/AC-3/E-AC-3 untested both directions, `codec/h265/export.rs` and the jitter buffer untested; `flv/import.rs` hard-aborts ingest on one negative-PTS frame (drop+warn would be kinder); `anyhow` still leaks through `ts::Export::next` / `flv::Import::decode` while the rest of the crate got typed errors; `container::Frame` gained a field without `#[non_exhaustive]`/constructor (repeats the break next time); orphaned+stale `unwrap_pts` doc comment; nondeterministic A/V tie-breaking via HashMap iteration order.
- moq-rtmp: `Conn` pub enum missing `#[non_exhaustive]` (has a feature-gated variant); eager S0/S1 before reading C0; unbounded accept-side `FuturesUnordered`; alive-but-silent publishers never reaped (keepalive only catches dead peers); `tokio` pulled with `features = ["full"]` in a library.
- moq-msf: draft-00 `isComplete`/`generatedAt` silently discarded (broadcast-termination semantics unimplementable); dangling `initRef` degrades to `init_data: None` with no warning; `Catalog` not `#[non_exhaustive]`; CHANGELOG describes removing a `Version` enum that never existed and the "Unreleased" section already shipped in 0.3.0.
- Swift/Kt packaging: kt Maven-probe inline bash belongs in `release.sh` and treats 5xx as "not published"; hardcoded `0\.2\.` grep must be bumped with the version range; stale `release-kt` concurrency group; root CLAUDE.md's "CI mirrors kt to moq-dev/moq-kotlin" claim is now wrong; `swift/justfile` comment describes the wrong script; README references a nonexistent `just check-ffi`. No Swift compile coverage in PR CI (same-PR drift between `swift/Sources` and `rs/moq-ffi` is never compile-checked; symbols were hand-verified in sync today).
- kio: `Consumer::write()` gives consumers mutable access on a type documented read-only, and re-exposes the `deref_mut`-always-marks-modified wake footgun to external users (the historical wasm freeze path via polls stays fixed).
- js: new exports in signals/net/watch documented with `//` instead of `/** */` (invisible on JSR); `TrackInfo` wire message defaults `timescale: 0` which every decoder rejects (make it required); `CatalogProducer` not re-exported from @moq/publish; `Broadcast.net` is a raw writable Signal where the new convention wants a read-only Getter.
- Python: `connect()` shorthand omits TLS pinning args; missing docstring on `Client.request_broadcast`; em dashes in new module docstrings (repo rule).

---

## API-shape verdicts (the "is the new surface good?" question)

**Good, keep:** the moq-net model reshape (`BroadcastInfo`/`TrackInfo`/`GroupInfo`/`FrameInfo` with inherited `Arc<Info>`, verified immutable-by-construction with no lock-order issues); two-phase `Server::accept_request` → `Request` with consume-self `ok()`/`close()`; `OriginPublish`/`BroadcastPublish` RAII guards; runtime `Timescale(NonZero)`; infallible `kio::Pending` subscribe/fetch (failure semantics stay distinguishable: `Unroutable`/`NotFound`/`Dropped`); the JS input/output signals convention; `TrackRequest.accept(info)` mirroring Rust exactly; @moq/flate extraction; moq-srt and moq-rtmp's shared `Config` + `run` + `Server`/`Request` pattern with consume-self terminal ops and zero third-party leakage; the relay `routes()`/`serve()` split (auth verified per-handler, can't be bypassed by merged routes); the TLS `Verification` unification (quiche gains roots/fingerprints; `http://` bootstrap can't weaken explicit pins); moq-video's `decode`/`encode` Config/Kind/Codec shapes.

**Fix before shipping:** moq-rtc's leaked str0m surface (the one large-scale violation); `GroupRequest`'s missing reject/drop semantics; the `#[non_exhaustive]` batch (moq-net `TrackInfo`/`Subscription`/`Fetch`/`BroadcastInfo`, moq-rtc Configs, moq-hls Configs, moq-rtmp `Conn`, moq-mux `container::Frame`); moq-srt `Server::bind(addr, latency)` positional option (a passphrase knob is already announced as "planned"); moq-hls's missing auth seam and pub `store` module.

---

## Filed as GitHub issues (findings verified on main too)

| Issue | Title |
|---|---|
| [#1999](https://github.com/moq-dev/moq/issues/1999) | moq-mux: fMP4 export drops composition-time offsets (B-frame PTS rewritten) |
| [#2000](https://github.com/moq-dev/moq/issues/2000) | moq-mux: FLV export writes PTS as tag timestamp, cts always 0 |
| [#2001](https://github.com/moq-dev/moq/issues/2001) | moq-mux/moq-srt: cross-timescale Timestamp arithmetic panics (3 sites) |
| [#2002](https://github.com/moq-dev/moq/issues/2002) | moq-msf: 0.3.0 rejects 0.2.0 catalogs with fractional jitter (released regression) |
| [#2003](https://github.com/moq-dev/moq/issues/2003) | hang catalog: displayAspectWidth vs displayRatioWidth never interop |
| [#2004](https://github.com/moq-dev/moq/issues/2004) | moq-rtc: default config fails every WHIP/WHEP accept with 500 |
| [#2005](https://github.com/moq-dev/moq/issues/2005) | moq-rtc: A/V sync has no NTP anchor (SR ignored; egress wallclock = dequeue time) |
| [#2006](https://github.com/moq-dev/moq/issues/2006) | moq-rtmp: play of nonexistent broadcast signals Play.Start then hangs forever |
| [#2007](https://github.com/moq-dev/moq/issues/2007) | moq-rtmp: E-RTMP codecs sent to legacy clients without negotiation |
| [#2008](https://github.com/moq-dev/moq/issues/2008) | moq-rtmp: publish dedup is per-run(), broken for RTMP+RTMPS dual listeners |
| [#2009](https://github.com/moq-dev/moq/issues/2009) | moq-hls: missing EXT-X-DISCONTINUITY-SEQUENCE; TARGETDURATION not constant |
| [#2010](https://github.com/moq-dev/moq/issues/2010) | moq-hls: Server broadcaster map never evicts |

---

## Suggested triage order on dev

1. C1 (js group cache), C2 (moq-boy), C3 (wasm panic): user-visible breakage, small fixes.
2. `poll_combined` merge semantics + `GroupRequest` reject: relay correctness, before anyone runs multi-viewer lite-05.
3. Main→dev merge (moq-mux staleness, qmux 0.1.3, HLS import fixes) + the two merge blockers (ALPN revert plan, quinn escape hatch).
4. FFI version-pin strategy + the Swift unbounded-stream fix, before any release workflow runs off dev.
5. The `#[non_exhaustive]` + options-struct batch while breaking changes are still free.
6. Doc sweep: py/kt/swift examples that throw, cli.md subscribe flags, stale capture build example.
