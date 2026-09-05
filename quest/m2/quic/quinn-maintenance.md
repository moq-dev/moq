# [S] Land the Quinn maintenance backlog

## Goal

The work in Quinn pull requests
[#2601](https://github.com/quinn-rs/quinn/pull/2601) and
[#2724](https://github.com/quinn-rs/quinn/pull/2724) is merged into Quinn main
or replaced by an explicitly linked upstream implementation. The old branches
and review threads no longer linger.

## Plan

As of 2026-09-02, #2601 is approved, mergeable, and green. Rebase it only when
main requires it, answer any new review, and ask a Quinn maintainer to merge.
noq already carries the equivalent as n0-computer/noq#667, so do not port it a
second time.

#2724 is green and has an approval, but GitHub still reports changes requested.
Resolve the outstanding review state, retain the regression for the first GSO
batch failing, rebase, request re-review, and follow it through merge. If noq
is selected, verify that the next Quinn sync contains the fix or send the same
small patch directly.

Do not keep a private rewrite merely to close the pull request. If upstream
chooses a different implementation, verify the reported memory and GSO retry
behaviors against that implementation, link it here, and retire the original
branch.
