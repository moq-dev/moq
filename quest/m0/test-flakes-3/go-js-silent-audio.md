# [S] go -> js audio plays its tone every night

## Goal

The interop matrix's `go -> js` cell stops failing intermittently with
silent Opus audio (`the opus rendition plays opus with 2 channels, per
channel rms 0`).

## Plan

Found by the 2026-11-20 triage of scheduled Interop runs: failed on 11-15,
11-17 and 11-19 (1 of 4 matrices on 11-16), passed on 11-18 and 11-20. It
passes locally, even with every subscriber pinned to 4 cores, and a one-time
1 s stall injected into the Go client did not cause silence.

Decided 2026-11-20 (maintainer, /quest-plan): ✅ plan it now, working from CI
traces (rejected: note it and wait for repeats).

- Pull the go-js trace from the next failing night's `interop-failure`
  artifact and compare the audio and video timestamps the player received,
  alongside the Go client's wall-clock write times and the player's arrival
  times. The tone's timestamps count samples written, so they stay contiguous
  even when ticks are dropped; only the wall-clock times can show a gap. Log
  the write times if the trace lacks them.
- The Go test client paces its tone with a `time.Ticker`, which drops ticks
  under load; Python and C++ pace against the start time. Switch it to
  start-anchored pacing, but don't claim that as the fix unless the write
  times show gaps the ticker should not have left.

Public API: none. Wire: none.
