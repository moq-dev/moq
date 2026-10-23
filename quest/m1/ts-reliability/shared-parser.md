# [L] One TS packet parser serves every program

## Goal

`--program all` scans the multiplex once: one packet parser owns the PAT,
the PID map and continuity, and dispatches each PID's packets to the
program that claims it. The cost of an MPTS ingest grows with input rate,
not with program count times input rate.

## Plan

Today `Programs` (`rs/moq-mux/src/container/ts/programs.rs`) feeds the same
bytes to an independent `Import` per program, so packet scanning, table
handling and health run once per program.

Decided 2026-10-22 by the maintainer: this lands before
[stream routes](/quest/m1/ts-reliability/stream-routes.md) and
[PAT changes](/quest/m1/ts-reliability/pat-lifecycle.md), whose plans work
around per-program reparsing (multiplex-wide PID rows moving between owners,
a removed program's `handle_pat` error stopping the whole ingest). Revisit
both plans once it lands. Re-confirmed 2026-10-22 after drafts of both opened
first: they rebase onto this and drop the accounting it replaces, such as the
`max()` merges in `Programs::stats` and PID rows moving between owners,
rather than landing ahead of it.

Add a benchmark in `rs/moq-mux/benches` that feeds `Programs` a generated
MPTS and sweeps program count and input rate independently; land it first
so the slope this removes shows.

Public API: none expected; report any change to `Programs`. Wire: none.
