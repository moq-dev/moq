# [M] Live PAT changes keep programs honest

## Goal

A live TS source whose PAT changes mid-stream keeps every broadcast meaning
one program. A moved PMT PID, a program added, and a program removed or
returning each have one observable outcome: followed, published, retired,
or published as a new epoch. None is silently ignored.

## Plan

What the code does today:

- A PAT naming another PMT PID for the program adds a reassembler but never
  drops the old one, and `program.pmtPid` keeps the first.
- `Programs` ignores a program a later PAT adds, without a warning. Its
  PIDs hold back the unclaimed-PID count for 0.5 s of program clock, then
  count as unclaimed in `ts::stats`, since no program reads them.
- Under `--program all`, the shared parser hands a later PAT that removes one
  program to every program's `Import`; the removed program's `handle_pat`
  errors, `Programs::decode` propagates it, and the whole multiplex ingest
  stops, including unrelated programs. `Programs` sees each whole PAT first
  (`Packet::pat`), so it can retire or start programs there instead. A
  program still waiting for its PMT is already dropped there, or moved to
  the PMT PID the later PAT names.
- A PAT naming another `transport_stream_id`, or a single-program import's
  PAT naming another program, already refuses the import.

Decided 2026-10-20 by the maintainer:

- A new PMT PID for the same program moves the reassembler and updates
  `program.pmtPid`, which the export follows with a new PAT. `Programs`
  publishes a program a later PAT adds as its own broadcast, as it does for
  the first PAT, rather than ignoring it.
- A later PAT that removes a program retires that program's broadcast: its
  tracks finish and it is unannounced, while the other programs carry on.
  If the program returns, it publishes a new epoch of its broadcast, never
  the retired instance's names for new content. A single-program import
  whose selected program leaves the PAT ends, as today.
- Open: a PAT that names a program whose PMT never arrives. `Programs`
  creates a program's broadcast only once its PMT lists a stream, so such a
  program waits holding only its PAT entry. Decide whether the import refuses
  it instead, and which clock bounds the wait (found in
  [moq-dev/moq#6122](https://github.com/moq-dev/moq/pull/6122)).

Rust: `rs/moq-mux` TS `Import::handle_pat` and `Programs`.

Tests use the generated `fixture::Mux` builder: a PMT PID move, and a
`--program all` MPTS whose PAT adds a program, drops one and later restores
it: the other program runs uninterrupted, and the returning one publishes a
new epoch.
