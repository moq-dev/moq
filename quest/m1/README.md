# m1: additive work for this cycle

## Goal

Improvements, fixes, and checks that need no semver-breaking release: each
lands on `main` and ships with the next cut, or as a `release` patch when it
is backported. Breaking work for the same cut lives in m2.

## Plan

Decided 2026-11-05 (maintainer, milestone re-cut): m1 is the additive half
of what was "everything before the cut" (2026-10-20). A quest stays here when
it would need no breaking release if it landed right after the cut; the rest
moved to [m2](/quest/m2/README.md), and m1's earlier decision log moved with
them, since it is the cut's. The cut no longer waits on m1: what lands before
it ships with it, and the rest ships at a later cut or as a `release` patch.

Placements decided in the re-cut: read-backfill (a JS fix that matches Rust),
buffered-ascending (lite-07 already carries the order field), webrtc-vendor
(internal, though it requires the m2 rename), and the crates.io trusted
publisher and OIDC quests (setup after the cut's first publishes) are
additive. video-vaapi moved up from later work
because a downstream iGPU transcode trial needs it now.

Held here by open claims: a quest's branch is its path, so a claimed quest
cannot move until its PR merges, and an epic moves with its children. These
breaking epics and quests stay in m1 for now and are listed in m2's
`Required`, so the cut still waits on them: api-audit, drop-set, rind, session-policy, and
stream-credit. The [re-cut follow-up](/quest/m2/recut-held.md)
moves what remains of them once their claims merge.

## Required

- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [Media fuzz findings](/quest/m1/fuzz-findings/README.md) - the nightly media fuzz targets run clean: mp4-atom, MKV block, TS step, SPS overflow, and catalog encoder bugs they found are fixed
- [A smaller, safer API than release](/quest/m1/api-audit/README.md) - every surface added since release has a consumer or is gone, overlapping operations are collapsed, names mirror across Rust, JS, and the bindings, and every break says how to migrate
- [moq-relay's tests pass without default features](/quest/m1/relay-no-default-features.md) - QUIC-only tests are gated on `quic`, and the nightly matrix runs the no-default build
- [moq-webrtc drops the ICE wire counters nothing reads](/quest/m1/ice-wire-counters.md) - the per-packet `Wire` atomics go once #8122 makes the ICE module private
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [A reader on a failed copy continues on the held re-offer](/quest/m1/copy-recover.md) - a live reader keeps a group whose copy fails by continuing on the same track's re-offer at the same frame
- [Group errors are cached by kind](/quest/m1/group-error-cache.md) - permanent group errors bind every reader, retryable ones are kept only for a bounded time, and a live SUBSCRIBE_DROP or FETCH answered `Old` no longer caches it for good
- [A route answer behind its idle copy's cache fails the copy](/quest/m1/answer-behind-cache.md) - a resuming copy whose route answers behind its cache fails loud instead of refusing the feed's groups as duplicates, so readers resubscribe onto a fresh copy
- [The overcommitted receive-window cells run again](/quest/m1/receive-windows-evicted.md) - `benchmark_receive_windows` measures its 256-stream and 4 MiB-frame cells again, reporting delivered frames and evictions instead of stalling
- [Stream credit by priority](/quest/m1/stream-credit/README.md) - every stream opens through one priority entry point, and a higher-priority group takes over or resets a lower one when credit runs out
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; time-stretched browser playback without steady-state underruns and a native trace replay remain
- [Native WebSocketStream keeps datagrams flowing](/quest/m1/websocket-stream-datagrams.md) - on Chromium's native `WebSocketStream`, QMux over WebSocket sheds a datagram only on a real send backlog, proven in headless Chromium
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [Push-to-talk's press-to-audible latency fits a budget on the defaults](/quest/m1/push-to-talk-latency.md) - press to audible on the browser defaults, excluding device output, stays under a kixtop-measured budget that `just test push-to-talk` enforces
- [Socket endpoints split into servers and clients](/quest/m1/socket-roles.md) - servers announce `.server/<id>` and clients `.client/<id>`, so plain grants keep a client from posing as a server or reading another client
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [Session policy lives in moq-net](/quest/m1/session-policy/README.md) - accept and connect deadlines, admission, window widening and reconnect live once in moq-net; moq-tokio and moq-relay's io_uring path call them
- [Vendor and trim the WebRTC stack](/quest/m1/webrtc-vendor.md) - str0m, dimpl, `is`, and sctp-proto live in moq-webrtc with unused code removed, so moq-webrtc can take rind
- [MoQ on rind](/quest/m1/rind/README.md) - every MoQ crate takes its crypto from rind, a pure-Rust graviola fork, so no build compiles C crypto
- [VA-API encode is correct on radeonsi](/quest/m1/vaapi-radeonsi.md) - AMD's VA-API H.264 encode decodes, or the node is refused for OpenH264
- [Tests run on a mocked clock](/quest/m1/mock-clock-tests/README.md) - about 300 tests stop proving nothing happened by waiting, sleeping to settle, or asserting on elapsed time
- [moq-mux reads H.265 SPS without scuffle](/quest/m1/h265-sps-reader.md) - moq-mux's own SPS reader replaces scuffle-h265 and scuffle-expgolomb, and the fuzz regressions still pass
- [QMux reads the app-limited flag on big-endian Linux](/quest/m1/qmux-app-limited-endian.md) - the send-rate filter's app-limited mask follows the target's byte order
- [Lifecycles are explicit state](/quest/m1/explicit-state/README.md) - every lifecycle the 2026-11-07 audit found implicit is one enum or union, fixing its 11 reachable bugs, with review and agent rules against new ones

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
