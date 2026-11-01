# [XS] Pin a quest that re-pins a queued PR

## Goal

moq's dev shell runs a moq-dev/quest whose `quest merge <pr> <token> --land
<sha>` moves an already-queued PR's `Quest-Land-Head` pin to the new head.
Merge sessions that run from this shell then never leave a stale queue entry
for the gate to reject. Check it by re-pinning a queued PR and reading its
auto-merge commit body.

## Plan

Decided 2026-11-01 (user, /quest-plan of the follow-ups from a moq-dev/quest
/quest-complete session):

- ✅ Plan the bump as its own quest: a pin bump that unblocks work is its own
  quest.
- The pinned quest (`7a191e6`, reporting 0.1.0) left
  [moq-dev/quest#290](https://github.com/moq-dev/quest/pull/290) and
  [moq-dev/quest#319](https://github.com/moq-dev/quest/pull/319) queued on
  their old heads after a push, and each needed a manual requeue.
- Bump `inputs.quest` in `flake.nix` to a current moq-dev/quest main. A newer
  quest also brings the land fixer's prompt and `quest merge --land` fixes.

## Related

- [Cheap checks skip the queue](/quest/m0/ci/gate-quick.md) - also waits on a pin that includes `quest gate --quick`
