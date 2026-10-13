# [S] Jobs skip setup when the diff selects no work

## Goal

A Check or Test job whose diff selects nothing for it, such as a quest-only
PR, finishes in well under a minute and still reports its required check as
passed. Jobs with work are unchanged.

## Plan

Observed: a quest-only Test job ran no tests but spent 101s freeing disk, 35s
restoring the Rust cache, and 79s entering Nix. The dispatcher in `sh/` is
already the one place that decides what a diff selects; ask it before setup
rather than keep a second path map in the workflow. It may need to run outside
the dev shell, or with a smaller one.

## Related

- [Self-hosted CI](/quest/m0/ci/self-hosted.md) - removes most of this setup on the host; hosted fallbacks and forks still pay it
