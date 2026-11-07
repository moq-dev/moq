# m0: immediate priorities

## Goal

The work in flight now: passing Nightly, Interop, and Video runs, a
draft-18 session to Cloudflare's relay that outlives its first request, backports
and patch releases for the `release` line, CI cost and latency, test flakes,
and a few small fixes for hostile input, WebSocket throughput, agent disk use,
and CRLF checkouts. The quest merge driver
([moq-dev/moq#6728](https://github.com/moq-dev/moq/pull/6728)), local reviews
([moq-dev/moq#7175](https://github.com/moq-dev/moq/pull/7175)), and the
declared HLS segment grid
([moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652)) landed.

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
- [A draft-18 session to Cloudflare outlives its first request](/quest/m0/cloudflare-draft18-session.md) - `moq fetch` against Cloudflare's draft-18 relay waits for its timeout instead of a `protocol violation`
- [JS qmux keeps credit and final sizes consistent with Rust](/quest/m0/qmux-js-credit.md) - a JS WebSocket session no longer stalls on cancelled streams or gets closed by a Rust peer with FINAL_SIZE_ERROR, and its control queue stays bounded
- [Peer and config values never panic or wrap moq-net arithmetic](/quest/m0/peer-arithmetic.md) - a peer's retry interval, object id, drop end or sequence, and huge configured durations no longer panic a task or wrap
- [Publish the release line's drain() wrappers](/quest/m0/ffi-shutdown-backport.md) - Dart `moq` 0.1.1 reaches pub.dev from `release`
- [Release's audio encoder rolls groups before the cap](/quest/m0/release-audio-group-cap.md) - a long `groupDuration` with short frames never throws `GroupTooLarge` on `release`
- [Local checks pass on unchanged main](/quest/m0/local-checks-main.md) - `sh/go/publish-wrapper.test.sh` passes under a global `tag.gpgsign = true`, so `just check` passes on a clean `main`
- [The invalid-attribute browser check matches moq-watch's attributes](/quest/m0/watch-invalid-attribute-check.md) - `publish-ui.browser.ts`'s invalid-attribute test passes on main again, with no stale `buffer` case
- [Bun-spawning test recipes install the JS workspace](/quest/m0/recipe-js-install.md) - `bare-fin`, `max-age`, `stream-limits`, `relay-fill`, and `receive-credit` pass in a fresh checkout
- [CI cost and latency](/quest/m0/ci/README.md) - cloud CI runs only in GitHub's merge queue, one tested entry at a time, expensive checks run nightly and gate `release`, caches move to R2, and runners are chosen last
- [More tests under load](/quest/m0/test-flakes-2/README.md) - load-only and queue failures, one quest per flake, fixed at the cause, since each one drops a queued PR
- [Tests can't wait on the real clock](/quest/m0/clock-guard/README.md) - a test whose real-clock timer fires fails every run, in JS and, if the spike holds, Rust
- [Agent build output can't fill the desktop's disk](/quest/m0/agent-disk-budget.md) - finished agent checkouts reclaim their mbx targets automatically, so parallel agent builds stop filling the root disk
