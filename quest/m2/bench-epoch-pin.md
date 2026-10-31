# [XS] moq-bench pins the instances it measures

## Goal

moq-bench requests each pooled broadcast pinned to the epoch it saw
announced, so a takeover between gathering and requesting is not counted as
the same broadcast.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7192](https://github.com/moq-dev/moq/pull/7192). Pass the
announced epoch to `request`. A pinned request that finds another instance is
refused; count that as a takeover, not a failure.

Public API: none. Wire: none.
