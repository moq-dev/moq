# [M] A signalled TS restart continues as a new epoch

## Goal

When a TS feed rewinds its time base and signals it
(`discontinuity_indicator` on the PCR PID, ISO/IEC 13818-1 2.4.3.5),
`moq import ts` and `moq-srt` finish the current broadcast cleanly and publish
the rest of the same input as a new broadcast under a fresh epoch, in the same
process and connection. This covers an encoder restart or source switch behind
a gateway that keeps its connection up, and a looping playout server, none of
which makes a new connection for the ingest gateways
to turn into an epoch. An unsignalled rewind stays fatal, as #4543 decided.

## Plan

Since #4543, every rewind ends the import with `TimestampRewind` from
`Producer::write`; the importer already reads the flag in `timebase_break`,
and a flagged forward jump publishes break markers and carries on.

Decided (maintainer, 2026-09-30):

- A rewind is new content, so it is always a new broadcast at a new epoch,
  never a continuation of the old one. The newest epoch wins the path, and
  viewers follow the `Restart` announce it produces (an end and start on
  older versions).
- `decode` stops at the flagged rewind and reports it. The caller finishes the
  old broadcast (a clean end, not an abort, so its viewers read to its end),
  publishes a new broadcast at the same path, minting a fresh epoch, and calls
  `import.restart(broadcast)`. The importer carries over only the PAT/PMT
  layout and the bytes it has not consumed, so there is no wait for the next
  PSI repetition; tracks, groups, and timestamps start fresh. `ts::Programs`
  does this per program. Returning unconsumed bytes to a fresh `Import` was
  rejected for the PSI wait, and letting the importer publish its own
  broadcast was rejected for coupling moq-mux to origin publishing.
- A flag with the next PCR behind the last one is a restart; a flag with a
  forward jump stays a break marker. The 33-bit wrap is modular, not a rewind.
- Callers: `moq import ts` (`rs/moq-cli`) and `moq-srt`'s `Publisher`, single
  program and `Program::All`.

Decided 2026-10-08: when the operator gave `--epoch`, a flagged rewind is
fatal like an unsignalled one, since minting a fresh epoch per rewind would
split a redundant pair. Restarting under the same epoch is not safe either:
the new run restarts group numbering, and a matching epoch lets viewers and
caches resume the old content under the same names. So the error says the
operator's supervisor must restart both hosts under a new shared epoch, and
`doc/bin/cli.md`'s redundant-pair example says the same.

Tests: a fixture with a flagged rewind publishes two broadcasts, the second
starting at the rewound PTS; the same rewind unflagged still errors; one SRT
connection carries both epochs. Update `doc/bin/cli.md` and `doc/bin/srt.md`.

Public API: breaking in moq-mux, `ts::Import::decode` reports a restart
and `restart` is new. Wire: none.

## Closes

- [#4582](https://github.com/moq-dev/moq/issues/4582) - a signalled backward TS discontinuity ends the import

## Related

- [Restart](/quest/m0/broadcast-epoch/restart.md) - the announce event viewers follow onto the new epoch
- [#5003](https://github.com/moq-dev/moq/pull/5003) - `import ts --passthrough`, which waits on this quest's `decode` change
