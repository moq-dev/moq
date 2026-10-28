# [S] Nightly's gate names a hung module

## Goal

The test that hung the 2026-10-27 scheduled Nightly gate
([job](https://github.com/moq-dev/moq/actions/runs/40506938673/job/122198526371),
36 minutes with no output before the timeout) is found and fixed at its
cause, and the gate prints per-module progress with timestamps, so a future
hang names its module.

## Plan

Found by [moq-dev/moq#6991](https://github.com/moq-dev/moq/pull/6991)'s
replay. The gate buffers output per module, so the log cannot say which one
hung. Reproduce with `just gate`, then add the timestamps. Never a retry or
a longer timeout.

Public API: none.
