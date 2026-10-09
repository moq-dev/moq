# [M] TS export lingers within an epoch and stitches across one only on request

## Goal

`moq export ts` and moq-srt's egress never splice two broadcasts into one TS
stream unless asked:

- `--linger <dur>` waits up to that long for the *same* broadcast (the same
  epoch) to come back after its route goes, and carries on with the same
  stream.
- A replacement (a newer epoch, or another instance of an epochless route)
  ends the export with an error that names `--stitch`. On an epochless route
  every return is a replacement.
- `--stitch` opts in to following a replacement. The caller drops its
  `ts::Export` and builds a new one on the new broadcast, with a full program
  switch: a new PMT from the new catalog, so tracks, codecs, and PIDs may
  change, plus PCR and continuity discontinuities. Nothing from the old
  broadcast survives except the output pipe or SRT connection.

Non-goal: identical output across a replacement. Two legs that are
byte-identical within an epoch may differ after one (#5101).

## Plan

Decided 2026-10-09. Today `--linger` (#4504) waits for the ended broadcast to
close and the path to route again, then calls `ts::Export::resume()`. That
call clears the decode clocks, jitter buffer, and schedule, but keeps the old
PMT and PIDs and matches the returned tracks by name only, so a renamed codec
goes out under the old stream_type and continuity counters run on unflagged.
That is a splice of whatever returns, assuming the layout matches without
checking. It also keys on errors and the broadcast closing, so an old
publisher that stays alive holds the export on a replaced broadcast.

- Delete `Export::resume()` and its partial reset.
- Drive both flags from announcements, as players do: `Start`, `Restart`, and
  `End` with their epochs. An export that fails while its broadcast is still
  announced exits 1 without lingering, as today.
- Same epoch within `--linger`: re-request it and keep the same `Export`.
  It is the same content, so the existing skip handling covers the gap (late
  frames drop, and a forward leap opens a new generation). Within an epoch,
  moq-net already resumes across routes, so this only matters once every
  route has gone.
- Without `--stitch`: subscriptions are sticky, so a `Restart` leaves the
  export on the old broadcast until it ends. If the path then serves a
  different instance, the export exits 1 with an error naming `--stitch`.
- With `--stitch`: on `Restart` (or `End` then `Start` with another
  instance) build a fresh `Export` at once. Every PID's first packet carries
  `discontinuity_indicator`, the PMT `version_number` advances (the only value
  handed from the old `Export`), and each elementary stream starts at a
  keyframe. Rejected: continuing only when the layout is identical, and
  deleting `--linger`. Exiting does not hide the switch from a receiver
  downstream of `tsp`, it only drops the flags.
- moq-srt's egress (`rs/moq-srt/src/ts.rs`) gets the same two options and keeps
  its SRT connection across a stitch. Its linger defaults to 0, like the CLI:
  an `End` that nothing replaces within the linger closes the SRT stream, so
  a finished broadcast never leaves a caller connected indefinitely.
- Open, decide at start: how the PMT version crosses to the new `Export`,
  since both callers live outside `moq-mux`. Recommended: a method that
  consumes the old export and returns its successor on a new source (such as
  `Export::stitch(self, source)`), so the version never becomes a public knob.
  Alternative: a builder option seeding the PSI version.
- Decided: the TS output has no epoch. A re-import of an export is out of
  scope.
- Fold in the stale wording from the marker audit: a marker group only
  declares a pause or forward break, since rewinds are refused (#3711). Rename
  `Consumer::restarts` / `ExportSource::restarts` for what they count, and fix
  the comments that say the publisher restarted or rewound its timeline
  (`rs/moq-mux/src/container/consumer.rs`, `rs/moq-srt/src/server.rs`).
- Tests, with mocked time:
  - A same-epoch return within the linger continues the stream with no PSI
    version change.
  - A replacement without `--stitch` exits 1, and an old publisher that stays
    alive keeps the export on it until it ends.
  - With `--stitch`, a replacement with a different codec and track set
    writes a new PMT version, flags every PID's first packet, starts each
    stream at a keyframe, and writes nothing from the old broadcast after the
    break.
  - Update #4504's relay-backed CLI tests, and add the stitch case for SRT
    egress, plus an unreplaced `End` closing the SRT stream at the linger.
- Docs: the `--linger` paragraph in `doc/bin/cli.md` (plus `--stitch`), any
  restart behavior in `doc/bin/srt.md`, and every example invocation using
  `--linger`.

Public API: breaking, `ts::Export::resume` is removed and `restarts()` is
renamed. New: the successor operation above. CLI: new `--stitch` on `export ts` and SRT egress, and `--linger`
no longer follows a replacement. Wire: none.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - the announce event and epoch the export follows

## Related

- [No stitch](/quest/m0/broadcast-epoch/no-stitch.md) - stops moq-net hiding a replacement from the export
- [Two TS export legs](/quest/m2/ts-hitless.md) - identical output within one epoch
