# [XS] moq-bench pins the instances it measures

## Goal

moq-bench requests each pooled broadcast pinned to the instance it saw
announced, so another instance taking the path between gathering and
requesting is not counted as the same broadcast.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7192](https://github.com/moq-dev/moq/pull/7192). Pass the
announced instance to `request`. A pinned request that finds another instance is
refused; count that as an instance change, not a failure.

Public API: none. Wire: none.
