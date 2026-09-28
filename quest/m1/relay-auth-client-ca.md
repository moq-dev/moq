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
  skips `validate` still cannot start. The exact shape (a bool, or something
  `init` already receives such as the listener TLS config) is open; prefer
  whatever makes the wrong call unrepresentable.
- `spawn_server` in `rs/moq-cli/src/main.rs` maps any `auth.validate()` error
  to `Auth::refuse`. Return the error instead, so a bad config stops startup.
- Update every caller, the tests #4364 added in both `moq-cli` and
  `moq-relay`, and `doc/bin/relay/auth.md` or `doc/lib/rs` wherever they name
  the methods.

Public API: breaks `moq_relay::auth::Config::validate` and `init`, removes
`validate_client_ca`, so this targets `dev`. Wire: none.

## Required

- #4364 merged to `main`
- `dev` has merged `main` after #4364 lands
