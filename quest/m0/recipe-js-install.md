# [XS] Bun-spawning test recipes install the JS workspace

## Goal

`just test bare-fin`, `max-age`, `stream-limits`, `relay-fill`, and
`receive-credit` pass in a fresh checkout. Each runs the root installer
before its Rust tests spawn `bun`, instead of relying on an earlier install.
`test/justfile` sets `working-directory := '.'`, so its recipes run in
`test/` and invoke it as `uv run --no-project python ../sh/js/install.py
frozen`, as [moq-dev/moq#7978](https://github.com/moq-dev/moq/pull/7978)'s
recipes do (CI's `interop.yml` "Workspace dependencies" step hides the gap today).

## Plan

Found by [moq-dev/moq#8104](https://github.com/moq-dev/moq/pull/8104): a fresh
checkout fails `just test bare-fin` with `Cannot find module '@moq/clock'`.
Use the coordinated installer from
[moq-dev/moq#7978](https://github.com/moq-dev/moq/pull/7978) rather than a raw
`bun install`, so the recipes share its lock and cache. Start once #7978 has
merged.
