# [M] Rust tests run when a file they read outside rs changes

## Goal

A change to any file a Rust test or build reads selects the Rust work that
reads it in `just check` and `just ci`, the way a change to the test does. One
check keeps it so: a new Rust read of a path the `rs` scope in
`sh/dispatch.sh` misses fails `just check`, naming the path. Today such a
change skips the Rust tests, so a broken read lands.

## Plan

Caused by [moq-dev/moq#8459](https://github.com/moq-dev/moq/pull/8459),
fixed by [moq-dev/moq#8486](https://github.com/moq-dev/moq/pull/8486): a new
`test/**.toml` broke a relay test that parses it, and the gate had scoped the
Rust tests out. #8486 moved the file but left the scope as it was. The
2026-11-11 and 2026-11-18 mistake audits in moq-dev/quest
([moq-dev/quest#641](https://github.com/moq-dev/quest/pull/641),
[moq-dev/quest#786](https://github.com/moq-dev/quest/pull/786)) both kept it.

Decided 2026-11-19 (maintainer): ✅ each read either moves into its crate or
is covered by widening the `rs` scope, with no hand-kept list of what a crate
reads (rejected: each crate declaring its outside reads in `Cargo.toml`;
moving every read in-crate as the only option).

Reads outside `rs/` that the `rs` scope misses today, found by grepping
`CARGO_MANIFEST_DIR` joins and `include_*!` paths:

- `test/receive-credit/sender.ts`, `test/max-age/client.ts`,
  `test/stream-limits/client.ts`, `test/relay-fill/relay.ts`, and
  `test/datagram-tracks/client.ts`, which moq-tokio's and moq-uring's
  integration tests run.
- `doc/concept/audio-jitter/{stretch,comfort}/*.json`, which moq-audio
  includes.
- moq-relay's `repository_configs_load`, which loads every tracked TOML
  under `demo/relay`, `packaging`, and `test`.

Recommended:

- Move a file into its crate when only Rust reads it. Widen the scope for a
  file that lives where it does for another reason, such as a TS script the
  JS side also checks, or a config a deployment ships. A widened path selects
  the whole workspace, since nothing else maps it to a crate. Narrow each
  widened pattern to the files read, such as `test/**.toml` rather than
  `test/`, and give the PR a count of how often those paths changed in recent
  history. That count is the gate's cost.
- `sh/rs/select.sh`'s seed from `nix/modules/moq-relay.nix` to moq-relay is
  the same kind of hand-kept mapping, so fold it into the same rule.
- The check finds the outside paths a Rust source names, whether through
  `CARGO_MANIFEST_DIR` or an `include_*!` path, and fails when the `rs` scope
  does not match one. `sh/rs/select.test.py` already checks that the scope
  covers targets outside their crate; extend that rather than add a script.
- Show that the check fails on #8459's shape, a new `test/**.toml`, before the
  fix, and passes after.
