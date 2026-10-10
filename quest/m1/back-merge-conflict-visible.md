# [XS] A back-merge that can't take a release says so on its PR

## Goal

When the Back-merge workflow can't merge a new `release` head into the open
back-merge branch because of a conflict, the open back-merge PR gets a comment
naming the release commit it couldn't take, so a human resolves it instead of
the failure sitting unread in an Actions run.

## Plan

Found in #5170 (2026-10-10): two Back-merge runs (for #5130 and #5135) failed
with `gh: Merge conflict (HTTP 409)` while #5150's branch was open, so those
backports reached `main` only through the next back-merge, #5176.
`sh/gh/back-merge.sh` merges the new release into the existing branch on
purpose, to keep any hand-resolved conflict, and fails loudly on a conflict.

Decided 2026-10-10: keep that design and keep failing, but make the failure
visible. On the 409, comment on the open back-merge PR with the release commit
and the instruction to merge `main` into the branch and resolve it, then exit
nonzero as today. Rejected: recreating the branch from `release`, which drops
hand-resolved work.

Public API: none. Wire: none.
