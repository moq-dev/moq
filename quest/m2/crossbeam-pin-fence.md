# [XS] Drop kio's epoch fence once crossbeam fixes pin

## Goal

Waiting on upstream crossbeam. Once
[crossbeam-rs/crossbeam#1352](https://github.com/crossbeam-rs/crossbeam/issues/1352)
is fixed in a released crossbeam-epoch, the workspace takes that release and
kio's `fence(Release)` in `Read::drop` (`rs/kio/src/log.rs`) is deleted, with
the log eviction loom model still passing on ARM.

Check #1352's state and crossbeam-epoch's latest release. Delete this quest if
upstream declines the fix.

## Plan

[moq-dev/moq#7134](https://github.com/moq-dev/moq/pull/7134) added the fence
to work around crossbeam-epoch 0.9.21's non-x86 `Local::pin`, which publishes
the epoch with a Relaxed store plus a SeqCst fence. Under the language memory
model that lets the collector free a segment while a read from the previous
pin is still unordered. The fence is free on x86 and one barrier per `Read` on
ARM.

Decided 2026-10-28 (maintainer): keep crossbeam-epoch rather than replace it.
[moq-dev/moq#6782](https://github.com/moq-dev/moq/pull/6782) chose it over
refcounted segments and per-read refcounts, it supports loom, and moka pulls
it into moq-relay anyway.

Verify on ARM: run `just rs loom` on an ARM host; Nightly no longer runs it
(decided 2026-10-31, see the [CI epic](/quest/m0/ci/README.md)). Without an
ARM host, force crossbeam's non-x86 branch in a scratch copy, as #7134 did,
and confirm the model passes without the fence.

Public API: none. Wire: none.
