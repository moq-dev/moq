# [XS] moq auth serve exposes a listener by its secret, not an allow flag

## Goal

`moq auth serve` accepts a non-loopback `--listen` address whenever
`--listen-secret` is set, and `--listen-public` is gone.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`rs/moq-cli/src/auth.rs` already refuses `--listen-public` without
`--listen-secret` and a non-loopback address without `--listen-public`; its
other use is the mesh guard, which
[moq-dev/moq#8756](https://github.com/moq-dev/moq/pull/8756) removes. After
that the flag carries nothing the address and the secret do not, and AGENTS.md
says not to add `allow-*` opt-out flags. Start once #8756 merges: delete the
flag and its setting, refuse a non-loopback address without a secret, update
`doc/bin/relay/auth.md`, and add a `MIGRATE.md` note, since `release` ships the
flag.

Public API: removes `moq auth serve --listen-public` and its config key.
Wire: none.
