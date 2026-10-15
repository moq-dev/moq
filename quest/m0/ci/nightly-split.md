# [S] Nightly finishes every step

## Goal

Every step of Nightly runs to completion, or fails on its own, within its
timeout. No step is skipped because an earlier one used up the job's hour.

## Plan

The latest sampled Nightly exhausted its 60 minutes after the feature checks
and several benchmarks, skipping 29 later steps. Split independent workloads
into parallel jobs and remove repeated builds before giving Nightly more
coverage. Raising the timeout is not the fix.
