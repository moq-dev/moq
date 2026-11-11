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
`Required`, so the cut still waits on them: drop-set and
stream-credit. The [re-cut follow-up](/quest/m2/recut-held.md)
moves what remains of them once their claims merge.

## Required

- [Drop set](/quest/m1/drop-set.md) - each cursor's disposition record is billed to the cache pool, as the track's drop runs are, so fragmenting drops costs what the pool holds; eviction folds final runs or ends the cursor with `TooFarBehind`
- [Routing guide](/quest/m1/routing-guide.md) - concepts and practical outbound federation setup
- [A reader on a failed copy continues on the held re-offer](/quest/m1/copy-recover.md) - a live reader keeps a group whose copy fails by continuing on the same track's re-offer at the same frame
- [Group errors are cached by kind](/quest/m1/group-error-cache.md) - permanent group errors bind every reader, retryable ones are kept only for a bounded time, and a live SUBSCRIBE_DROP or FETCH answered `Old` no longer caches it for good
- [The overcommitted receive-window cells run again](/quest/m1/receive-windows-evicted.md) - `benchmark_receive_windows` measures its 256-stream and 4 MiB-frame cells again, reporting delivered frames and evictions instead of stalling
- [Stream credit by priority](/quest/m1/stream-credit/README.md) - every stream opens through one priority entry point, and a higher-priority group takes over or resets a lower one when credit runs out
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target is default in both languages; audio that slows down and catches up around it in every player, graded in that direction, and a native trace replay remain
- [Buffered playback subscribes in ascending order](/quest/m1/buffered-ascending.md) - a buffered `<moq-watch>` asks for the oldest group first, so a voice reply's opening arrives first, and audio's head hold shrinks to the floor
- [Lifecycles are explicit state](/quest/m1/explicit-state/README.md) - every lifecycle the 2026-11-07 audit found implicit is one enum or union, fixing the bugs in the 11 lifecycles it redesigns, with review and agent rules against new ones
- [Track drop runs are forgotten below the floor](/quest/m1/drop-runs-prune.md) - a track's dropped-group record stays proportional to its window in Rust and JS
- [A resumed QUIC client gives up on its own idle timer](/quest/m1/quic-resume-idle.md) - a client stopped past its idle timeout closes within about one timeout of resuming, without waiting on a stateless reset
- [A reused moq-transport alias delivers a group that beats its SUBSCRIBE_OK](/quest/m1/ietf-alias-reuse.md) - a new SUBSCRIBE clears its track's alias tombstone, so moqxr's alias-0 catalog refetch no longer hangs
- [A screen share publishes only while joined](/quest/m1/room-screen-joined.md) - `@moq/room` publishes no screen share before joining or after leaving
- [The TLA+ models run nightly and reviews resync them](/quest/m1/model-checks-nightly.md) - every model config runs in Nightly, open findings must still fail, and review reruns a model whose covered code changed
- [The route model covers a mesh with clients and an announce budget](/quest/m1/route-mesh-model.md) - every ordering on 4-5 relay meshes with two origins and clients, with an announce budget and convergence
- [Seeded lite-07 meshes hold the route rules and an announce budget](/quest/m1/route-mesh-sweep.md) - the route rules and message budget checked on real sessions over random meshes
- [A model of a relay chain's failover delivers every group once](/quest/m1/failover-chain-model.md) - route, resume, and subscribe composed across a relay chain whose hops fail
- [A reused moq-transport alias refuses its track at once](/quest/m1/ietf-alias-refuse.md) - a SUBSCRIBE_OK on an alias already bound fails its readers and removes its entry without waiting on the peer's FIN acknowledgement
- [Unused crates, scripts, and files are deleted](/quest/m1/unused-code.md) - moq-room, the moq-native tombstone, demo/throttle, sh/dart/publish.sh, demo/web's Solid deps, and unpublished packages' release notes go
- [The CARLA bridge builds outside the root workspace](/quest/m1/carla-workspace.md) - workspace builds stop compiling the demo bridge
- [A client's first dial fails fast](/quest/m1/reconnect-fail-fast.md) - a client that never got a session stops instead of retrying, later failures back off, and a version mismatch is logged on both sides
- [Find out whether Cloudflare's relay really sends SOLICIT](/quest/m1/cf-solicit.md) - capture its SETUP, then close moq-dev/moq#8418 as their bug or fix our parsing

## Related

- [A busy js/net serve yields](https://github.com/moq-dev/moq/pull/5289) - js/net's serve loop leaves the browser its event loop under a fast publisher, if it does not already
