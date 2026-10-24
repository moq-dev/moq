# [S] JS reload takes the Rust backoff config

## Goal

`@moq/net`'s reload loop takes the same backoff settings as Rust (`initial`,
`max`, and `healthy`, the time a session must stay up to count as healthy)
and accepts no option it ignores.

## Plan

Rust's moq-tokio split its healthy bar out of `backoff.initial` into
`backoff.healthy` ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529)). `ReloadDelay`
(`js/net/src/connection/reload.ts`) still uses `initial` as the healthy bar.
Add `healthy` with Rust's default and a mocked-time test.

`ReloadProps` is `Omit<ConnectProps, ...>`, so it accepts `extensions`, but
the reload loop never passes them to `connect()`. Decided 2026-10-23
(maintainer): one quest for both. Omit `extensions` from `ReloadProps`, since
[moq-net exports](/quest/m1/api-audit/net-exports.md) makes it `@internal`,
unless that quest lands keeping it public, in which case forward it.

Public API: breaking JS option change, with a `MIGRATE.md` entry. Wire: none.
