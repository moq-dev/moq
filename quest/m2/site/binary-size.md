# [S] Cap committed binary size

## Goal

A check in the merge queue (`just ci check`) rejects any binary file, such as
an image, gif, video, or font, that a PR adds or grows past 500 KB. It looks
at the diff only, so existing files are not flagged. An explicit allowlist
covers today's oversized fixtures.

## Plan

Decided 2026-10-20 in planning:

- **500 KB, repository-wide.** Git keeps every version of a binary forever,
  so the cap protects history size for everyone who clones. It applies to the
  whole tree, not only `site/`, since a fixture or doc image bloats history
  just as much. 1 MB and 250 KB were rejected as too loose and too strict.
- **Only files the PR adds or grows.** A file is flagged when its new size is
  over the cap and it is new or larger than on the base. Shrinking or
  touching an existing oversized file passes. Every tracked file was rejected
  as a scope: it would fail on fixtures nobody is changing.
- **An explicit allowlist** names the exceptions, each with a one-line reason.
  As of 2026-10-20 the only tracked binaries over the cap are two MPEG-TS test
  fixtures, `rs/moq-mux/src/container/ts/test_data/scte35/kyrion_dirtystart.ts`
  (1.4 MB) and `rs/moq-mux/src/container/ts/test_data/kyrion_mpeg2av_ac3.ts`
  (941 KB). Re-verify with `git ls-tree -r -l HEAD` before landing; some
  large tracked files are generated text, which the check ignores.

Guidance:

- Wire it into the impact map in `sh/dispatch.sh` as a repository-wide lint
  (an empty pattern), next to `markdown` and `shell`, reusing the base that
  `sh/changed.sh` resolves.
- Let git decide what is binary (`git diff --numstat` reports `-` for binary
  files) rather than keeping an extension list, so a new format is covered.
- A `--all` run has no diff; decide whether it skips or checks every tracked
  binary against the cap and allowlist. Either way it must pass today.
- The failure names each file, its size, and the cap, and says to recompress
  or allowlist it with a reason.
- Add a regression test beside the other `sh/` tests that a too-large added
  binary fails, an allowlisted one passes, and a shrinking one passes.
