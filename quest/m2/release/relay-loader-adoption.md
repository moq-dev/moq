# [M] Embedders load relay config through the shared loader

## Goal

Condition: an embedding consumer's moq-relay pin includes
[moq-dev/moq#6535](https://github.com/moq-dev/moq/pull/6535), which adds
`Config::merge(&CliLayer)`. Check each consumer's lockfile for a moq-relay
version or revision that carries it, and advance a pin that lacks it through
that consumer's normal upstream-first process.

Then every embedding consumer loads its relay config through
`Config::merge`, an explicit obsolete file or environment spelling refuses
its startup, and its embedded CLI-only fields survive the merge. Delete this
quest once every embedder is migrated.

## Plan

Split from the relay config API audit (maintainer, 2026-10-23): the upstream
loader landed first, because migrating a consumer before its pin carries the
API would break it.

For each embedding consumer:

1. Replace its direct TOML parsing plus `update_from` (or hand-built
   `merge_into`) setup with `config.relay.merge(&cli_layer)?`.
2. Migrate the renamed settings and native types listed in the Impact
   section of [moq-dev/moq#6535](https://github.com/moq-dev/moq/pull/6535),
   in its config files, environment, and code.
3. Verify with that consumer's own checks that an explicit obsolete file or
   environment spelling refuses startup, and that its root CLI-only fields
   survive, reading `Config::source` provenance rather than comparing against
   default values.

Do not add compatibility aliases upstream, and do not combine unrelated
pending transport configuration changes with a consumer's migration.
