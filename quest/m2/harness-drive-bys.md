# [XS] Decide the relay listening kind field

## Goal

The test harness isolation from #3509 keeps its point (private run
directories, reserved ports, process-group teardown) and sheds the one thing
that came along without a reason to live here if nobody reads it: the `kind`
field on the relay's `listening` log line.

## Plan

- Decide whether the relay's `kind = "quic" | "http" | "https"` on the
  `listening` line and the two new web-listener lines stay. They are
  operator-visible and documented in `doc/bin/relay/config.md`; keep them
  only if the harness actually reads the bound address from them, otherwise
  revert to the single line and drop the doc.
- The `just worktree` recipe that rode in with the same PR is deleted by
  [Thin justfiles](/quest/m2/tooling/justfiles.md), not here.

## Related

- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - the harness run directory this trims around
- [Thin justfiles](/quest/m2/tooling/justfiles.md) - removes the `worktree` recipe
