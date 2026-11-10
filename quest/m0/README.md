# m0: immediate priorities

## Goal

The work in flight now: passing Nightly, Interop, and Video runs, backports
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

- [The C bindings accept MoqInstance again](/quest/m0/check-c-moqinstance.md) - `check c` passes on main once the C generator stops refusing `MoqInstance`'s exported `Debug`
- [Nightly passes on main](/quest/m0/nightly/README.md) - the scheduled Nightly passes end to end, so red means a new break
- [Interop and Video pass on main](/quest/m0/interop-green/README.md) - the scheduled Interop and Video workflows pass, so red means a new break
- [@moq/clock's Instant has no throwing Duration arithmetic, mirroring kio](/quest/m0/peer-arithmetic.md) - JS `Instant` drops `add`/`sub` for checked and saturating forms, so each call site states what overflow means, as kio's do
- [Publish the release line's drain() wrappers](/quest/m0/ffi-shutdown-backport.md) - Dart `moq` 0.1.1 reaches pub.dev from `release`
- [CI cost and latency](/quest/m0/ci/README.md) - cloud CI runs only in GitHub's merge queue, one tested entry at a time, expensive checks run nightly and gate `release`, caches move to R2, and runners are chosen last
- [More tests under load](/quest/m0/test-flakes-2/README.md) - load-only and queue failures, one quest per flake, fixed at the cause, since each one drops a queued PR
- [just check passes on macOS](/quest/m0/macos-check.md) - the version test, the doc re-export link test, and the carla bridge docs pass on a macOS host
- [Agent build output can't fill the desktop's disk](/quest/m0/agent-disk-budget.md) - finished agent checkouts reclaim their mbx targets automatically, so parallel agent builds stop filling the root disk
