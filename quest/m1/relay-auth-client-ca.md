# [S] moq-relay auth validate takes the client-CA flag

## Goal

On dev, no caller of `moq_relay::auth::Config` can start with `--auth-public`
rules alongside a listener TLS client CA by forgetting a check.
`Config::validate` takes the client-CA flag, `init` requires it too, and
`validate_client_ca` is gone. The `moq` CLI fails loud on an invalid auth
config instead of quietly refusing every session.

## Plan

- [#4364](https://github.com/moq-dev/moq/pull/4364) adds an additive
  `Config::validate_client_ca(&self, client_ca: bool)` that `Relay::load` and
  the CLI must each remember to call; the CLI missing the original check is
  the bug it fixes. Fold it into `validate(&self, client_ca: bool)` so every
  caller has to answer, and have `init` take the same answer so a caller that
  skips `validate` still cannot start. `init` today only receives the
  outbound auth TLS, so the listener's client-CA answer is a new input; its
  shape (a bool, or the listener TLS config) is open. Prefer whatever makes
  the wrong call unrepresentable.
- `spawn_server` in `rs/moq-cli/src/main.rs` maps any `auth.validate()` error
  to `Auth::refuse`. That fallback is only right for a LAN-only mesh with no
  auth configured, which `MoqSide::validate` permits and whose peers admit
  through the cluster. Make that case explicit and let any other error stop
  startup.
- A listener TLS client CA on a stream-only relay (no QUIC owner at all,
  the `NoBackend` case in `rs/moq-relay/src/relay.rs`) is never checked, so
  refuse that config at load instead of silently ignoring the CA. Worker-owned
  QUIC (`quic_owned_elsewhere`) still enforces the CA, so keep accepting it.
- Update every caller, the tests #4364 added in both `moq-cli` and
  `moq-relay`, and `doc/bin/relay/auth.md` or `doc/lib/rs` wherever they name
  the methods.

Public API: breaks `moq_relay::auth::Config::validate` and `init`, removes
`validate_client_ca`, so this targets `dev`. Wire: none.

## Required

- #4364 merged to `main`
- `dev` has merged `main` after #4364 lands
