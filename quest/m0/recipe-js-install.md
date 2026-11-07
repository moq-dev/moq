# [XS] Bun-spawning test recipes install the JS workspace

## Goal

`just test bare-fin`, `max-age`, `stream-limits`, `relay-fill`, and
`receive-credit` pass in a fresh checkout. Each runs the root installer
`sh/js/install.py frozen` (from `test/justfile`, a path relative to the
repository root, not `test/`) before its Rust tests spawn `bun`, instead of relying on an earlier install
(CI's `interop.yml` "Workspace dependencies" step hides the gap today).

## Plan

Found by [moq-dev/moq#8104](https://github.com/moq-dev/moq/pull/8104): a fresh
checkout fails `just test bare-fin` with `Cannot find module '@moq/clock'`.
Use the coordinated installer from
[moq-dev/moq#7978](https://github.com/moq-dev/moq/pull/7978) rather than a raw
`bun install`, so the recipes share its lock and cache. Start once #7978 has
merged.
