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
additive. [video-vaapi](/quest/m1/video-vaapi.md) moved up from later work
because a downstream iGPU transcode trial needs it now.

Held here by open claims: a quest's branch is its path, so a claimed quest
cannot move until its PR merges, and an epic moves with its children. These
breaking epics and quests stay in m1 for now and are listed in m2's
`Required`, so the cut still waits on them: api-audit, datagram-tracks, drop-set, rind, session-policy, and
stream-credit. The [re-cut follow-up](/quest/m2/recut-held.md)
moves what remains of them once their claims merge. release-perf, held for
the same reason, gates the cut through the
[cut gate](/quest/m2/cut-gate/README.md).

## Required

- [Drop set](/quest/m1/drop-set.md) - each cursor judges missing groups against its own budget, so a narrow reader drops promptly and a widening re-offers groups already reported dropped on lite-07
- [Media fuzz findings](/quest/m1/fuzz-findings/README.md) - the nightly media fuzz targets run clean: mp4-atom, MKV block, TS step, SPS overflow, and catalog encoder bugs they found are fixed
- [A smaller, safer API than release](/quest/m1/api-audit/README.md) - every surface added since release has a consumer or is gone, overlapping operations are collapsed, names mirror across Rust, JS, and the bindings, and every break says how to migrate
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [A reader on a failed copy continues on the held re-offer](/quest/m1/copy-recover.md) - a live reader keeps a group whose copy fails by continuing on the same track's re-offer at the same frame
- [Group errors are cached by kind](/quest/m1/group-error-cache.md) - permanent group errors bind every reader, retryable ones are kept only for a bounded time, and a live SUBSCRIBE_DROP or FETCH answered `Old` no longer caches it for good
- [A route answer behind its idle copy's cache fails the copy](/quest/m1/answer-behind-cache.md) - a resuming copy whose route answers behind its cache fails loud instead of refusing the feed's groups as duplicates, so readers resubscribe onto a fresh copy
- [The overcommitted receive-window cells run again](/quest/m1/receive-windows-evicted.md) - `benchmark_receive_windows` measures its 256-stream and 4 MiB-frame cells again, reporting delivered frames and evictions instead of stalling
- [Datagram tracks](/quest/m1/datagram-tracks/README.md) - a track is datagrams or streams; datagram tracks have their own request and sequence, no ranges, no cache, and map to object ID 0 on moq-transport
- [Stream credit by priority](/quest/m1/stream-credit/README.md) - every stream opens through one priority entry point, and a higher-priority group takes over or resets a lower one when credit runs out
- [Sockets](/quest/m1/socket/README.md) - a generic dial/accept `Socket` in moq-net, Rust, JS, and Python, with tracks as streams; moq-boy and CARLA move onto it
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; time-stretched browser playback without steady-state underruns and a native trace replay remain
- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS container consumer drops groups below its read cursor like Rust
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [Push-to-talk loses no speech](/quest/m1/push-to-talk/README.md) - an unmute is heard from its first frame with no catalog round trip: mute moves from holding a disabled rendition to suppressed silence with comfort noise, after a route spike; capture delivers frames on a steady cadence
- [Push-to-talk's press-to-audible latency fits a budget on the defaults](/quest/m1/push-to-talk-latency.md) - press to audible on the browser defaults, excluding device output, stays under a kixtop-measured budget that `just test push-to-talk` enforces
- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - both modes have explicit profiles, automated validation, loss tests, and a dedicated guide
- [WebGPU on Safari](/quest/m1/webgpu-safari.md) - the WebGPU renderer is verified on Safari 26 for macOS and iOS
- [Main is no slower than release](/quest/m1/release-perf/README.md) - every benchmark where main is slower than release is fixed or accepted with a reason, checked by a final paired run before the cut
- [Session policy lives in moq-net](/quest/m1/session-policy/README.md) - accept and connect deadlines, admission, window widening and reconnect live once in moq-net; moq-tokio and moq-relay's io_uring path call them
- [Vendor and trim the WebRTC stack](/quest/m1/webrtc-vendor.md) - str0m, dimpl, `is`, and sctp-proto live in moq-webrtc with unused code removed, so moq-rtc can take rind
- [MoQ on rind](/quest/m1/rind/README.md) - every MoQ crate takes its crypto from rind, a pure-Rust graviola fork, so no build compiles C crypto
- [VAAPI encode and decode](/quest/m1/video-vaapi.md) - H.265 encode and decode, pre-generated bindings, and pooled resize surfaces, including the moq-dev/vaapi release that carries them
- [Read a texture back without an encoder](/quest/m1/video-readback.md) - moq-video copies a wgpu texture into packed RGBA with no encoder probe, for the studio's Linux virtual camera

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
