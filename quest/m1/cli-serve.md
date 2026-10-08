# [S] A listening CLI session is counted and drained like a relay's

## Goal

A session accepted by `moq import --listen` or `moq export --listen` is
counted in stats and drained on shutdown, the way a relay's is. Admission
already matches the relay: since #3688 `serve_client`
(`rs/moq-cli/src/main.rs`) admits each session through a `moq_relay::auth`
lease, scopes the origin by its grant, and refuses what the grant does not
allow. What remains is that it then calls `moq_relay::supervise` with
`shutdown::Observer::disabled()` and no stats. `moq-relay` stays its own
minimal binary; this does not make `moq` the relay.

## Plan

Decided in the 2026-10-05 audit: re-scoped to what remains. The `spawn_server`
auth path now refuses only a LAN-only mesh with no auth source and stops
startup on any other invalid auth config.

- Drain: on SIGTERM the listener sends GOAWAY and waits `drain_timeout` like
  the relay, through a real `shutdown::Observer` instead of the disabled one.
  [Drain handshakes](/quest/m1/drain-handshakes.md) moves where the relay
  takes that count; hand the observer over the way it settles.
- Stats: `MoqSide` nests `stats::Config` like the relay, so `--stats-*` reads
  the same on both binaries, and `supervise` gets it instead of `None`.
- Docs: `doc/bin/cli.md` describes listening in the relay's terms and links
  the relay's auth page rather than restating it.
- Test: a listening import drains its viewers on SIGTERM within
  `drain_timeout`, and its sessions show up in stats.

Decided in the 2026-09-30 audit: no longer waits on
[`moq relay`](/quest/m3/moq-relay-subcommand.md).

## Related

- [`moq relay`](/quest/m3/moq-relay-subcommand.md) - the CLI later hosts the whole relay
- [Drain handshakes](/quest/m1/drain-handshakes.md) - moves where the drain count is taken
