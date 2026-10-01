# [S] Admission walk benchmark

## Goal

A benchmark sweeps tracks per front against copies per track for the
driver's admission walk, so a per-event cost that grows with the front
shows up as a slope.

## Plan

On the wildcard line, `run_front` (`rs/moq-net/src/model/origin.rs`) walks
every track and every copy (`io.copies()`) and calls `Provenance::admit` after
each event whenever `front.admit()` returns an origin, even when nothing
changed; Abort and End walk the same way. #4279 left this unbenchmarked
(moq-dev/moq#4607 covered pool resolution). The walk only runs once a
lite-07 reply names an origin, so extend `rs/moq-net/benches/session.rs` over
lite-07 mock sessions. If the slope matters, say so rather than optimizing
here; [Front deadlines](/quest/m1/front-deadline-index.md) owns per-track
wakes.

## Required

- The wildcard line (moq-dev/moq#4403) lands on main, where the walk lives
