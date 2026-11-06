# [XS] The relay bench reports io_uring pool size and RSS

## Goal

The `bench/` relay summary reports each run's io_uring buffer pool gauges
(receive and send, total and in use) and the relay's RSS, so a regression in
pool growth shows up in normal bench runs.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7233](https://github.com/moq-dev/moq/pull/7233), which
added the gauges to `/metrics` and measured them with a throwaway script.
Read them from the relay's `/metrics` at the end of each workload.
