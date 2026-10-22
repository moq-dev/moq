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
