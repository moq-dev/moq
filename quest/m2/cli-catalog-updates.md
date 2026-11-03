# [S] The moq CLI can publish a changing catalog

## Goal

`moq publish` can keep a broadcast's catalog open and publish later catalog
updates, such as adding or removing a track mid-stream, without restarting
the broadcast or its epoch. An operator can then replay catalog recovery
cases (a relay restart while a catalog group is open, a reader joining or
resuming mid-update) with stock tools instead of hand-built clients.

## Plan

Decided 2026-11-03 (maintainer, `/quest-plan`): m2. A relay-replacement
replay of catalog resume had to build scratch publisher clients because the
CLI publishes one catalog per broadcast (`rs/moq-cli/src/publish.rs`,
`PublishCatalog`).

- Pick the smallest interface that drives updates from outside: for example a
  catalog file the CLI watches, or catalog updates on a control stream. Ask
  the maintainer if the choice changes the CLI's public surface.
- Keep the one-catalog default unchanged.
- Test a mid-stream update that a subscribed reader sees exactly once, and a
  reader joining after the update.
