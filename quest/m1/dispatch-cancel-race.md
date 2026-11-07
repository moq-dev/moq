# [S] A cancelled dispatch lane stops its phase promptly

## Goal

Terminating a `sh/dispatch.sh` run stops each lane's running phase promptly,
even when the group TERM lands after a lane's phase subshell checks for a
pending trap but before it forks the phase's command. A test forces that
window and proves the phase's command gets the signal.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from a follow-up of
[moq-dev/moq#8138](https://github.com/moq-dev/moq/pull/8138). It was found by
reading Bash's source, not reproduced. In the lane shell (`sh/dispatch.sh`,
the `trap 'exit 143' TERM` around the phase), a TERM arriving between the
pending-trap check and the fork of the phase's command never reaches that
command. The deferred trap then runs only once the whole phase exits, so a
cancel can take as long as the phase. 8138's terminate test does not hit it,
because its phase is already running when TERM arrives.

Reproduce it first, by forcing the window as 8138 forced its own (for example
a hook that sends the group TERM right before the fork) on the pinned Bash.
Then close the window at its cause, for example by having the lane resend its
pending TERM to the phase's process group once the fork returns. No retry
loop or timeout. Extend `sh/dispatch-query.test.py` with the forced case.
