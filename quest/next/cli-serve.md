# [M] A listening CLI session is admitted like a relay's

## Goal

A session accepted by `moq import --listen` or `moq export --listen` is
authenticated (JWT, mTLS, or public prefixes), scoped by its grant, counted in
stats, and drained on shutdown, the way a relay admits one. Today
`spawn_server`, `route_server`, and `spawn_serve` (`rs/moq-cli/src/main.rs`)
accept every request with `request.ok()` and attach the process's single
origin in whichever directions the stages use, so a listening CLI is an open
relay. `moq-relay` stays its own minimal binary; this does not make `moq` the
relay.

## Plan

- Replace the CLI's three server helpers with the relay's acceptance path,
  exposing a small library entry point when the CLI needs it. `MoqSide` nests
  `moq_relay::auth::Config` and `stats::Config`, so `--auth-*` and `--stats-*`
  read the same on both binaries; `--auth-public '**'` is the open listener,
  spelled the same on both.
- The Unix peer-credential gate on `--listen-unix-bind` and the certificate
  endpoint for an explicit `--listen` survive, rehomed on the relay's web
  server or kept as the CLI's, whichever leaves one copy.
- Drain: on SIGTERM the listener sends GOAWAY and waits `drain_timeout` like
  the relay, instead of dropping sessions.
- Docs: `doc/bin/cli.md` describes listening in the relay's terms and links
  the relay's auth page rather than restating it.
- Test: an authenticated viewer with a scoped token sees only its root on a
  `moq import --listen`; an unscoped one is refused when a key is
  configured; the unauthenticated import-to-export smoke keeps passing with
  `--auth-public`.

## Required

- [`moq relay`](/quest/next/moq-relay-subcommand.md) - the CLI hosts the relay library, which `serve` comes from
