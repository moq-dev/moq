# [M] moq export: a linger rides out a broadcast that leaves and returns

## Goal

Every `moq export <format>` can outlive a session bounce or a publisher
restart. `--linger <duration>` (default 0) is how long the exporter waits for
the broadcast to be announced again after it goes away; when it returns, the
output resumes after a discontinuity, and while it is gone the output simply
stops (no stuffing, so carrier and content liveness stay one event). The exit
code says how the broadcast ended: 0 when the catalog track finished cleanly,
1 when it was dropped or anything else failed. Today every export subscribes
once and exits 1 with `json: dropped`, even while `moq_tokio` is mid-reconnect
(#3926).

## Plan

- A catalog FIN and a drop both wait out the linger, so a planned publisher
  restart is covered; when the wait expires, the exit code reflects the last
  end. With the default of 0, a FIN exits 0 and a drop exits 1 immediately.
- One implementation in the shared export path: `run_stdout` in
  `rs/moq-cli/src/main.rs` and `moq_mux::Source`, not per format. Re-resolve
  with `origin::Consumer::routed_broadcast` after the old broadcast ends; the
  origin already outlives each session.
- A return is a new catalog on the same exporter: each format writes it the
  way it already writes a catalog change or discontinuity (TS sets the
  discontinuity indicator and re-emits PSI). A format that cannot express a
  restart refuses a non-zero `--linger` at startup.
- `drive()` currently maps every clean task end and every error the same way;
  thread the FIN versus drop distinction through to the exit code.
- `doc/bin/cli.md`: document `--linger` and the exit codes next to
  `export --max-age`.
- Tests: a relay-backed CLI test that restarts the publisher within the linger
  and sees output resume, one that lets it expire and checks exit 1, and a
  clean FIN that exits 0.

## Closes

- [#3926](https://github.com/moq-dev/moq/issues/3926) - close this issue when the quest finishes
