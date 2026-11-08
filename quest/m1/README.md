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
`Required`, so the cut still waits on them: drop-set, rind, and
stream-credit. The [re-cut follow-up](/quest/m2/recut-held.md)
moves what remains of them once their claims merge.

## Required

- [Drop set](/quest/m1/drop-set.md) - the drop set's track and cursor runs are billed to the cache pool, so fragmenting drops costs what the pool holds; eviction folds final runs or ends the cursor with `TooFarBehind`
- [js/net and moq-net decode the same inputs the same way](/quest/m1/js-net-input-checks.md) - a shared input corpus pins both decoders, and a typed endpoint stops loopback spellings from slipping past GOAWAY checks
- [Rust and JS run one token and key-set corpus](/quest/m1/token-parse-strict.md) - a shared corpus pins both parsers, so a null `exp` no longer means never-expiring in Rust
- [Resetting a kio sleep to its deadline does not wake it](/quest/m1/kio-reset-same-deadline.md) - a caller that resets on every poll no longer spins the simulator
- [A cluster peer's identity is a type, not a string](/quest/m1/cluster-peer-identity.md) - a canonical `PeerKey` and a refused duplicate cluster id stop self-dials, double links, and silent mesh partitions
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [A reader on a failed copy continues on the held re-offer](/quest/m1/copy-recover.md) - a live reader keeps a group whose copy fails by continuing on the same track's re-offer at the same frame
- [Group errors are cached by kind](/quest/m1/group-error-cache.md) - permanent group errors bind every reader, retryable ones are kept only for a bounded time, and a live SUBSCRIBE_DROP or FETCH answered `Old` no longer caches it for good
- [The overcommitted receive-window cells run again](/quest/m1/receive-windows-evicted.md) - `benchmark_receive_windows` measures its 256-stream and 4 MiB-frame cells again, reporting delivered frames and evictions instead of stalling
- [Stream credit by priority](/quest/m1/stream-credit/README.md) - every stream opens through one priority entry point, and a higher-priority group takes over or resets a lower one when credit runs out
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; time-stretched browser playback without steady-state underruns and a native trace replay remain
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [One assets() for every hosted file](/quest/m1/hosted-assets.md) - `@moq/hang` owns `assets()`, which also points the libav polyfill at the hosted wasm
- [Watch sources default to the decoder's probe](/quest/m1/watch-supported-default.md) - a source built without `in.supported` plays instead of selecting nothing
- [Push-to-talk's press-to-audible latency fits a budget on the defaults](/quest/m1/push-to-talk-latency.md) - press to audible on the browser defaults, excluding device output, stays under a kixtop-measured budget that `just test push-to-talk` enforces
- [MoQ on rind](/quest/m1/rind/README.md) - every MoQ crate takes its crypto from rind, a pure-Rust graviola fork, so no build compiles C crypto
- [Tests run on a mocked clock](/quest/m1/mock-clock-tests/README.md) - about 300 tests stop proving nothing happened by waiting, sleeping to settle, or asserting on elapsed time
- [moq-mux reads only the base layer's H.265 SPS](/quest/m1/h265-base-layer-sps.md) - a layered MV-HEVC or SHVC stream imports as its base layer instead of misreading an enhancement-layer SPS, which still reaches the track
- [Lifecycles are explicit state](/quest/m1/explicit-state/README.md) - every lifecycle the 2026-11-07 audit found implicit is one enum or union, fixing the bugs in the 11 lifecycles it redesigns, with review and agent rules against new ones
- [Session auth grants and replies stay consistent and bounded](/quest/m1/auth-stream-bounds.md) - an AUTH_ERROR revokes its grant, queued grants and retiring tokens are bounded, and dropping queued requests cannot deadlock
- [Track drop runs are forgotten below the floor](/quest/m1/drop-runs-prune.md) - a track's dropped-group record stays proportional to its window in Rust and JS
- [A macOS camera delivers frames the renderer imports](/quest/m1/camera-nv12-macos.md) - AVFoundation capture asks for NV12, so a camera whose native format is `2vuy` reaches the renderer
- [Encoders keyframe by time, not frame count](/quest/m1/keyframe-by-time.md) - groups stay within the declared join interval when a source's frame rate drifts from the configured one

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
