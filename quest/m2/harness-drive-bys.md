# [XS] Revert the relay listening kind field

## Goal

The test harness isolation from #3509 keeps its point (private run
directories, reserved ports, process-group teardown) and sheds the one thing
that came along without a reason to live here: the `kind` field on the
relay's `listening` log line. Nothing under `test/` reads it, so it goes.

## Plan

- `rs/moq-relay/src/relay.rs:266` logs
  `tracing::info!(%addr, kind = "quic", "listening")`; revert it to the
  single `tracing::info!(%addr, "listening")` line #3509 replaced.
- `rs/moq-relay/src/web.rs:328-337` adds a helper that logs each web
  listener's bound address with `kind`; drop the field there too.
- `doc/bin/relay/config.md:234-249` documents the three `kind=` records;
  drop the block.
- The `just worktree` recipe that rode in with the same PR is deleted by
  [Thin justfiles](/quest/m2/tooling/justfiles.md), not here.

## Related

- [Failure artifacts](/quest/m2/qa-failure-artifacts.md) - the harness run directory this trims around
- [Thin justfiles](/quest/m2/tooling/justfiles.md) - removes the `worktree` recipe
