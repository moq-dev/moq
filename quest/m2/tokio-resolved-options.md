# [S] moq-tokio's resolved config reports "off" as None

## Goal

moq-tokio's resolved values and accessors say "off" with `Option`, not zero,
finishing what
[moq-dev/moq#8265](https://github.com/moq-dev/moq/pull/8265) does for moq-net:
`connect::Resolved.timeout`, `connection::Backoff::{max, timeout}()`, the
GOAWAY handover, and `quic::Resolved.idle_timeout`. The text knobs (CLI,
TOML, env, FFI, C) keep `0` = off, since they need three states and absence
already means "use the default"; the conversion happens once, where they
resolve.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8265](https://github.com/moq-dev/moq/pull/8265), which makes
moq-net's reconnect policy take `Option` and keeps moq-tokio's config layer
on zero). Milestone: m2, since it breaks moq-tokio's public API; start after
#8265 merges.

On `main`: `Backoff::max()` documents "zero leaves the delay uncapped" and
`timeout()` "zero meaning unlimited" (`rs/moq-tokio/src/connection.rs`);
`connect::Resolved.timeout` is a `Duration` whose `--connect-timeout` says
"zero disables this duration" (`rs/moq-tokio/src/connect.rs`); the handover
knob says the same; and `quic::Resolved.idle_timeout` turns zero into `None`
only when it builds the transport (`rs/moq-tokio/src/quic.rs`). Audit the
other `Resolved` structs for the same pattern while there.

Public API: moq-tokio's resolved fields and accessors become `Option`.
Migrate: compare against `None` instead of `Duration::ZERO`. Wire: none.
