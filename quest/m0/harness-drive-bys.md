# [S] Trim what rode in with the harness isolation

## Goal

The test harness isolation from #3509 keeps its point (private run
directories, reserved ports, process-group teardown) and sheds the two
things that came along without a reason to live in this repository: the
`just worktree` recipe and, if nobody reads it, the `kind` field on the
relay's `listening` log line.

## Plan

- Delete the `worktree` recipe and its helpers from the root `justfile`
  (about 265 lines: metadata-access probing, base recording, staleness
  reports) and the section of `test/README.md` that documents it. Keep the
  `_base` split that `_changed` shares, since `just check` scopes with it.
- Decide whether the relay's `kind = "quic" | "http" | "https"` on the
  `listening` line and the two new web-listener lines stay. They are
  operator-visible and documented in `doc/bin/relay/config.md`; keep them
  only if the harness actually reads the bound address from them, otherwise
  revert to the single line and drop the doc.
- `just clean` staying this-checkout-only is fine; leave it.

## Related

- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - the harness run directory this trims around
