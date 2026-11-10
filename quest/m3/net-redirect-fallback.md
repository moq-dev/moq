# [S] A dead redirect target falls back to the configured relay

## Goal

After `@moq/net` follows a GOAWAY redirect and the target dies, redials do not
stay on that dead target forever: they return to the configured URL, which
still answers.

## Plan

Found 2026-11-10 by the failover chart ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`js/watch/verify/Redirect.cfg` breaks `RedialsALiveRelay` in 4 steps. In
`js/net/src/connection/reload.ts` every redial after a redirect targets only
the relay it was sent to, with no retry timeout by default, so the viewer
redials a dead relay indefinitely. `residual: a redirected connection redials
only its dead target` in `js/net/src/connection/migrate.test.ts` pins it.
moq-relay never names a URI in its GOAWAY, so today only third-party relays
or a same-host port move trigger it.

Recommended fix: a failed dial to a redirect target drops it and redials the
configured URL. Compare with Rust's `Reconnect` (`rs/moq-net/src/reconnect.rs`),
whose model in `verify-reconnect` covers which address a redial picks, and
keep both languages on one rule. Flip the residual test and update the
Failover chart.

Caused by: no single PR found; redirect targets have been sticky since
js/net followed GOAWAY URIs.

Public API: none. Wire: none.
