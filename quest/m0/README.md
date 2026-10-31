# m0: immediate priorities

## Goal

The work in flight now: a passing Nightly, fixes for the `release` line, the
quest merge driver, local reviews, CI cost and latency, and test flakes.

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
- [Release credential leaks](/quest/m0/security-audit/README.md) - tokens stay out of logs and outbound HTTP clients verify their peer, on `main` and `release`
- [Interop and Video pass on main](/quest/m0/interop-green/README.md) - the scheduled Interop and Video workflows pass, so red means a new break
- [Release shutdown backport](/quest/m0/ffi-shutdown-backport.md) - `release` publishes a non-breaking moq-ffi with an async `drain()` and qmux sessions on the QUIC stream limit
- [Backport audio groupDuration](/quest/m0/backport-audio-group-duration.md) - `@moq/publish` 0.5.4 on `release` can pack several Opus frames per group, making Cloudflare's relay drop audio less often
- [Backport the SETUP-path and iroh-key fixes](/quest/m0/backport-security-fixes.md) - `release` refuses a conflicting SETUP Path and keeps persisted iroh keys private, in one patch release
- [Backport the joined-head FETCH race fix](/quest/m0/backport-fill-head-race.md) - a `release` relay stops FETCHing a group head its own SUBSCRIBE is bringing, which left live readers hanging
- [CI cost and latency](/quest/m0/ci/README.md) - cloud CI runs only in a batched merge gate as one `just ci` pass per batch, expensive checks run nightly and gate `release`, caches move to R2, and runners are chosen last
- [More tests under load](/quest/m0/test-flakes-2/README.md) - load-only and queue failures, one quest per flake, fixed at the cause, since each one fails a whole batch
- [Backport the dot-segment and auth-redirect refusals](/quest/m0/backport-release-hardening.md) - `release`'s relay refuses dot-segment paths and its auth client refuses redirects
