# m0: immediate priorities

## Goal

The work in flight now: a passing Nightly, fixes for the `release` line, the
quest merge driver, local reviews, CI cost and latency, test flakes, and
replacing the catalog's HLS records with a declared segment grid. That last
one is a breaking catalog and API change, pulled ahead of m1's other planned
format changes; it lands on `main` and ships with the next cut.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done.

Per-session request caps landed in #4820, idle fronts in #5054, and the
lost demand-poll wake in #5091.

Routing: Wildcard landed in #4403, so a service claims the prefix it could
serve instead of enumerating broadcasts. Serving the relay's ingested-only
view (`origin::Consumer::local()`) to localhost workers belongs to moq.pro's
edge, which embeds moq-relay; it moved there on 2026-09-28.

## Required

- [Nightly passes on main](/quest/m0/nightly/README.md) - the scheduled Nightly passes end to end, so red means a new break
- [Interop and Video pass on main](/quest/m0/interop-green/README.md) - the scheduled Interop and Video workflows pass, so red means a new break
- [moq-video shares moq-mux's bounded H.265 SPS walk](/quest/m0/video-sps-walk.md) - a hostile publisher's H.265 SPS can't panic a subscriber's decoder
- [Publish the release line's drain() wrappers](/quest/m0/ffi-shutdown-backport.md) - Python `moq-rs` 0.5.2 and Dart `moq` 0.1.1 reach PyPI and pub.dev from `release`
- [Backport audio groupDuration](/quest/m0/backport-audio-group-duration.md) - `@moq/publish` 0.5.4 on `release` can pack several Opus frames per group, making Cloudflare's relay drop audio less often
- [Backport the SETUP-path and iroh-key fixes](/quest/m0/backport-security-fixes.md) - `release` refuses a conflicting SETUP Path and keeps persisted iroh keys private, in one patch release
- [Backport the joined-head FETCH race fix](/quest/m0/backport-fill-head-race.md) - a `release` relay stops FETCHing a group head its own SUBSCRIBE is bringing, which left live readers hanging
- [CI cost and latency](/quest/m0/ci/README.md) - cloud CI runs only in GitHub's merge queue, one tested entry at a time, expensive checks run nightly and gate `release`, caches move to R2, and runners are chosen last
- [More tests under load](/quest/m0/test-flakes-2/README.md) - load-only and queue failures, one quest per flake, fixed at the cause, since each one fails a whole batch
- [Benchmark sessions can't overlap on a host](/quest/m0/bench-host-lock.md) - a host-level lock keeps a second benchmark or profile session from measuring under the first's load
- [A waiter's record limit changes no behavior](/quest/m0/kio-waiter-records.md) - two moq-net tests stop failing when kio's waiters remember 16 lists, and the limit becomes a tuning knob worth 3.9% per delivered group
- [Backport the relay and auth hardening to release](/quest/m0/backport-release-hardening.md) - `release`'s relay refuses dot-segment paths, serves its HTTP routes with main's response hardening, and withdraws advertisements a peer starves of send credit; its auth client refuses redirects
- [quest init keeps moq's nix-wrapped worktree hooks](/quest/m0/quest-worktree-hooks.md) - quest pin bumps keep the hooks that run moq's pinned `quest`
- [HLS lists contributions](/quest/m0/hls-contributions.md) - an HLS master lists each catalog contribution's renditions, and a contribution's media is subscribed only while players fetch it
- [@moq/net passes qmux stream limits through](/quest/m0/qmux-stream-limits.md) - Node and WebSocket JS clients stop failing near 100 live tracks
