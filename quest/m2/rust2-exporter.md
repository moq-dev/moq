# [XS] Waiting: rust2's exporter is ready for moq

## Goal

rust2's exporter generates every language moq ships from moq's crates, so
[the switch](/quest/m2/ffi-exporter.md) can start. Delete this quest once the
condition holds, so the switch resurfaces.

## Waiting

- moq-dev/rust2: [its exporter epic](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export/README.md)
  finishes with a tag or flake revision moq can pin, and moq-ffi built on it
  can still publish to crates.io (no git dependency). Check that rust2's
  `quest/m1/ffi-export/README.md` is gone from its `main` and its README lists
  the generators.

## Plan

Decided 2026-11-02 (maintainer, /quest-unblock): this replaces the earlier wait on rust2 publishing its UniFFI
fork. rust2's `quest/m1/uniffi-release.md` is dropped with it: the fork is not
published.
