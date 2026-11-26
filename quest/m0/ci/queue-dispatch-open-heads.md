# [XS] Queue (pull request) dispatches report only on open PR heads

## Goal

`gh workflow run queue-pr.yml -f sha=<sha>` posts the skipped `Queue` check
run only when `<sha>` is the head of an open pull request into `main`, and
fails the run otherwise. A merge group's commit, or any other commit, can then
never get a skipped `Queue` that would pass the group before its build reports.

## Plan

- ✅ keep (maintainer, 2026-11-25) (rejected: skip it), in
  https://github.com/moq-dev/moq/pull/9659#issuecomment-7179934222.
- Before posting, the dispatch job looks the commit up with
  `gh api repos/$REPO/commits/$SHA/pulls` and requires an open PR whose
  `head.sha` is `$SHA` and whose `base.ref` is `main`; anything else exits
  non-zero with an `::error::`. The job adds `pull-requests: read` beside
  `checks: write`, since a job's explicit permissions grant nothing else.
- Extend `sh/gh/queue.test.ts`: run the step against a stub `gh` and assert it
  posts for an open main PR head and refuses a merge group commit
  (`gh-readonly-queue/...` head with no open PR) and a closed PR's head.
- Only users with write access can dispatch, so this guards against a mistake,
  not an outside attacker; the check costs one API call per dispatch.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the merge queue this context gates
