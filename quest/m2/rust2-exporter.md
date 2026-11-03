# [XS] Waiting: rust2's exporter is ready for moq

## Goal

Condition: [rust2's exporter epic](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-export/README.md)
generates every language moq ships from moq's crates and has a tag or flake
revision moq can pin, and moq-ffi built on it can still publish to crates.io
(no git dependency). Check: rust2's `quest/m1/ffi-export/README.md` is gone from
its `main`, and its README lists the generators. Delete this quest once both
hold, so [the switch](/quest/m2/ffi-exporter.md) resurfaces.

## Plan

Decided 2026-11-02 (maintainer, /quest-unblock): this replaces the earlier wait on rust2 publishing its UniFFI
fork. rust2's `quest/m1/uniffi-release.md` is dropped with it: the fork is not
published.
