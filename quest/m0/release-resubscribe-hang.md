# [S] Release resubscribe hang

## Goal

It is settled whether `release` has a moq-lite-06 bug where a subscription that
is closed and reopened never reaches the relay. If the bug is real, a fix with
a regression test is backported to `release`, which moq.pro runs. If it is a
harness artifact, the reason is recorded and this quest is deleted.

## Plan

Found while backporting main's benches to `release` in
[#6103](https://github.com/moq-dev/moq/pull/6103): `session_churn_*/moq-lite-06`
hangs on `release`. After one subscription opens and closes, the reopened
subscription never reaches the relay. The same bench passes on main.

1. Reproduce it on `origin/release` as a session test (one subscriber; open,
   close, and reopen the same track through a relay). Mock time, not sleeps.
2. If it hangs, find the cause. Land the test on main first, per the
   main-first backport rule in `CONTRIBUTING.md`, then port it to `release`
   in a separate PR. If main fixed the bug, bisect for that fix and port it
   with the test. If main never had it (the code diverged), the `release` PR
   carries the minimal fix for code that no longer exists on main.
3. If the test passes on `release`, the bench harness's backport is at fault
   (release's `tests/support` mock differs from main's). Record that in the PR
   and delete this quest.

Verification, 2026-10-22: release `36f392f492` passes a paused-time session
reproducer that opens, closes, and reopens the track three times and checks a
fresh frame each time. The no-read benchmark pattern times out waiting for
demand even on its first iteration. `track::Consumer::subscribe` in release
registers a spliced subscription per segment only when its reader is polled;
awaiting it only resolves track info. Cached info makes a later open return
without any new TRACK request, so it never asks the publisher for data.

The benchmark now drives the reader until the publisher reports a registered
subscription, on both main and release for a comparable workload. It produces
no groups, so polling starts the reader without consuming content. A generic
used edge can also mean a TRACK query; the registered subscription proves the
SUBSCRIBE itself arrived.
No production fix or wire/API change is needed. Remaining work is to remove
this completed quest and its links; automatic approval review refused that
action under the no-abandonment restriction, including after the completion
requirement above was supplied.
