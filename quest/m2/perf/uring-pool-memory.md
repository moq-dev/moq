# [S] io_uring buffer pool memory

## Goal

A measured verdict on how much memory the io_uring workers' UDP buffer
pools hold, reserved versus resident, across connection counts, bursts,
sustained load, and after a burst ends. A shrink policy lands only if the
retained memory matters.

## Plan

The pools grow and never shrink, by design (`rs/moq-uring/src/udp.rs`
module docs). Per socket, so per worker: 16 receive buffers of ~64 KiB
(~1 MiB, touched) and 64 send buffers of 64 KiB (4 MiB reserved, touched
when written) at start, up to 256 receive and 1024 send buffers (~80 MiB).
A receive flood or a load spike ratchets each worker toward that ceiling.
The worker-turn benches use 2 KiB buffers, so none measures this.

Add pool capacity and occupancy to the worker metrics, then run the relay at
production sizing on the fanout and chat shapes, sweeping connections and
workers, and record RSS and pool capacity during and after a burst. A
no-cost result is recorded and closes the quest.

Decided 2026-10-22 from the io_uring audit.

Public API: none. Wire: none.

## Required

- [Profile the io_uring workers](/quest/m2/perf/uring-profile.md) - the recipe this measures with
