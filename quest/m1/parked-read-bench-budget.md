# [XS] track_parked_read never outlives its budget

## Goal

`rs/moq-net/benches/track.rs`'s `track_parked_read` completes at default
Criterion settings on any machine, instead of panicking once warm-up runs
long enough for parked reads to expire.

## Plan

Found while benchmarking #5159 (2026-10-09): each append advances timestamps
by 2.5 ms, so a warm-up past about 1.44M iterations crosses the bench's 3600 s
age budget and the parked reads expire, panicking at the bench's assertion.
It fails on unchanged `main`; whether it hits depends on machine speed. Size
the budget or the timeline to the iteration count (or reset state per batch)
so the measured path stays the parked one.
