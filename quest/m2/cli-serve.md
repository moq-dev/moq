# [L] The CLI serves like a relay

## Goal

The only difference between `moq` and `moq-relay` is that the relay listens by
default. A `moq --listen` session is authenticated (JWT, mTLS, or public
prefixes), scoped by its token, recorded as a hop, counted in stats, and
drained on shutdown, exactly as on a relay. The CLI's stages are a local
publisher and subscriber on that same origin.

## Plan

Today `spawn_server`, `route_server`, and `spawn_serve`
(`rs/moq-cli/src/main.rs`) accept every request with `request.ok()`, attach
the process's single origin in whichever directions the stages use, and never
look at a path or token. The relay's `serve` and `Connection::run`
(`rs/moq-relay/src/relay.rs`, `connection.rs`) do everything listed in the
goal. Once [one LAN mesh](/quest/m1/lan-mesh.md) lands the CLI's origin is the
cluster's, so the relay's `serve` can take over the listener directly.

- Replace the CLI's three server helpers with `moq_relay::serve` over the
  cluster, the auth, and the shutdown handle. `MoqSide` nests
  `moq_relay::AuthConfig` and `StatsConfig`, so `--auth-*` and `--stats-*`
  read the same on both binaries.
- Default grant: with no auth configured the CLI keeps what it gives today, an
  open listener over the whole origin, spelled as the relay's public prefixes
  covering the root. The relay's own default stays a refusal, since that is
  its `no auth-key, auth-key-dir, auth-api, or public path configured` check;
  the difference is the listening default this quest is about, and both defaults
  are documented side by side.
- The Unix peer-credential gate on `--listen-unix-bind` and the certificate
  endpoint `web::run_web` spawns for an explicit `--listen` survive, rehomed
  on the relay's web server or kept as the CLI's, whichever leaves one copy.
- Drain: `moq import --listen` on SIGTERM sends GOAWAY and waits
  `drain_timeout` like the relay, instead of dropping sessions.
- Docs: `doc/bin/cli.md` describes serving in the relay's terms and links the
  relay's auth page rather than restating it.
- Test: an authenticated viewer with a scoped token sees only its root on a
  `moq import --listen`, and an unscoped one is refused when a key is
  configured; the existing unauthenticated import-to-export smoke keeps
  passing with the default grant.

## Required

- [One LAN mesh](/quest/m1/lan-mesh.md) - the CLI's origin becomes the cluster's
- [`moq relay`](/quest/m2/moq-relay-subcommand.md) - the CLI hosts the whole relay library
