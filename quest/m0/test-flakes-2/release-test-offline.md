# [XS] release.test.ts never reaches the network

## Goal

`js/common/release.test.ts` runs without network access and never times out
on a live registry lookup, so it can't drop an unrelated PR from the merge
queue.

## Plan

Decided 2026-11-05 (maintainer, follow-up from the queue drop of
[moq-dev/moq#7867](https://github.com/moq-dev/moq/pull/7867)): ✅ m0.

`the JSR token reaches only deno publish` timed out at 5 s in a
`merge_group` build: its fixture runs `release.ts` with `jsr=true` and
`dryRun=false`, which makes a live jsr.io metadata lookup. The land fixer
reproduced the timeout under `unshare -rn`. Inject that lookup, the way the
test already fakes the publish commands, so the whole suite passes offline.
