# [M] TS export starts a fresh stream on a replaced broadcast

## Goal

When the broadcast behind `moq export ts --linger` or moq-srt's egress is
replaced (an announce `Restart`, or `End` then `Start` on older versions), the
caller drops its `ts::Export` and builds a new one on the new broadcast.
Nothing from the old broadcast survives except the output pipe or SRT
connection. The new stream is marked in-band, so a receiver resets on it. A
TS output never tries to survive an epoch or route change.

Non-goal: identical output across a replacement. Two legs that are
byte-identical within an epoch may differ after one (#5101).

## Plan

Decided 2026-10-09. Today `--linger` (#4504) waits for the ended broadcast to
close and the path to route again, then calls `ts::Export::resume()`, which
clears the decode clocks, jitter buffer, and schedule but keeps the same
`Export`. It keys on errors and the broadcast closing, so an old publisher
that stays alive keeps the export on the replaced broadcast.

- Delete `Export::resume()` and its partial reset. A replacement builds a
  fresh `Export`, so no state can carry over by construction.
- Drive the restart from announcements, as players do: `Restart`, or `End`
  then `Start`. An export that fails while its broadcast is still announced
  exits 1 without lingering, as today.
- Mark the break in-band: each PID's first packet in the new stream carries
  `discontinuity_indicator` (the PCR's time-base break and every continuity
  counter restart), and PAT/PMT `version_number` advances. That version is
  the only value handed from the old `Export` to the new one.
- moq-srt's egress (`rs/moq-srt/src/ts.rs`) follows the same rule and keeps its
  SRT connection across the break.
- Decided: the TS output has no epoch. A re-import of an export is out of
  scope.
- Tests, with mocked time: an export across a `Restart` writes no unit from
  the old broadcast after the break, flags every PID's first packet, and
  advances the PAT/PMT version. An old publisher that stays alive does not
  hold the export once a newer epoch is announced. Update #4504's
  relay-backed CLI tests, and add the same case for SRT egress.
- Docs: the `--linger` paragraph in `doc/bin/cli.md` and any restart
  behavior in `doc/bin/srt.md`.

Public API: breaking, `ts::Export::resume` is removed. Wire: none.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - the announce event the export follows

## Related

- [No stitch](/quest/m0/broadcast-epoch/no-stitch.md) - stops moq-net hiding a replacement from the export
- [Two TS export legs](/quest/m2/ts-hitless.md) - identical output within one epoch
