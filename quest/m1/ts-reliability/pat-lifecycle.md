# [S] A live export follows a moved PMT PID

## Goal

A running `export ts` whose catalog moves `program.pmtPid` follows it: a PAT
at the next `version_number` names the new PID before any PMT goes out
there, and nothing goes out on the old PID after it. The output stays one
program throughout, and no two PATs at one version name different PIDs.

## Plan

The import follows the PAT: a moved PMT PID moves its reassembler and
`program.pmtPid`, and `Programs` publishes, retires and republishes programs
as the PAT adds, drops and restores them. A running export still keeps the
PMT on the PID its tables first went out on, whatever `program.pmtPid` later
records: its output stays consistent, but a move is followed only by an
export started after it.

The maintainer's plan, from closing
[moq-dev/moq#6301](https://github.com/moq-dev/moq/pull/6301): the export
follows a new `program.pmtPid` with a new PAT, built on the table rework
that lays the PAT and PMT from the current table state when their slot is
laid instead of riding queued units, so a move is one more state change
rather than a rewrite of queued copies. That rework has not landed.

The draft then says that `pmtPid` can change in place, and when a consumer
rebuilding the stream moves the PMT.

Rust: `rs/moq-mux` TS export (`pmt_pid`, `build_psi`, the PSI schedule).

Tests: a live export whose catalog moves `pmtPid` emits PATs at versions 0
then 1, with nothing on the new PID before the moving PAT and nothing on the
old one after it, and a move while every copy is still queued goes out as
one PAT naming the new PID. `revision_test`'s
`a_track_recorded_on_the_pmt_pid_after_the_catalog_moves_it_takes_another`
then pins the move instead of the PMT staying put.
