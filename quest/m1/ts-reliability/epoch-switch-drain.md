# [M] A stitched switch keeps playing the old instance

## Goal

When `moq export ts --stitch` follows a newer epoch, the output keeps playing
the old instance until the new one's clock is acquired, then switches with the
marked program discontinuity it has today. A switch away from a healthy leg,
such as a redundant importer starting or restarting, leaves no hole in the
output. A switch after a failure costs only what the failed leg did not
deliver. Today `Export::followed`
(`rs/moq-mux/src/container/ts/export.rs`) replaces the whole export on a
switch, discarding the old leg's held backlog (up to one `--delay`) and
holding output while the new clock is acquired.

Out of scope: changing route order, timestamp continuity across epochs, and
hitless failover between importers.

## Plan

Decided 2026-10-26 (maintainer, importing
[moq-dev/moq#6877](https://github.com/moq-dev/moq/issues/6877)):

- Routing stays as decided on 2026-10-19: the newest epoch wins, so starting
  or restarting a backup importer is a switch. Rejected: a standby epoch that
  routes only when no live epoch remains (a second, publisher-declared
  selection policy ahead of epoch on the wire), and receiver-select in the CLI
  (a multi-broadcast `export ts`). Separate paths chosen by the subscriber
  stay documented in `doc/setup/redundancy.md`.
- The new instance opens a new jitter generation instead of a new export,
  reusing the generation machinery `rs/moq-mux/src/jitter.rs` already has for
  in-source timeline restarts: generations go out in turn, and the new clock
  is never ahead of a deadline already given out.
- The old instance's subscription keeps feeding its generation until the new
  generation is acquired, bounded by the `--follow` window, then stops.
  Rejected: playing out only what the buffer holds at the switch.
- Distinct epochs promise no timestamp relation, so the switch stays a program
  change: new PMT version, every PID flagged, PCR discontinuity. Do not align
  the legs' timestamps; that is the cross-importer alignment dropped on
  2026-10-19.
- The redundant failover integration test in
  `rs/moq-cli/tests/integration/export.rs` prints each switch's output gap
  without asserting it. Assert that a switch away from a healthy leg leaves
  no gap, behind a bufferbloat impairment.
- Fix inline: `doc/setup/redundancy.md` and `doc/bin/cli.md` say the switch
  is not hitless and that starting a backup causes it; say what a switch now
  costs.

## Closes

- [#6877](https://github.com/moq-dev/moq/issues/6877) - a redundant ingest pair: let a standby epoch wait behind a live one, and let a follower switch without emptying its delay
