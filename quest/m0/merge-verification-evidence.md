# [M] Verification evidence for the merge candidate

## Goal

A reviewer gets one compact record of what was tested, on which source and
environment, and what remains unverified. Results from an older head, a different
binary, or a stale base cannot authorize the current merge candidate.

## Plan

`just _changed` prints its selected base and includes untracked files, while
GitHub's Check workflow tests a PR checkout. There is no shared receipt joining
local results, dirty source, binary overrides, optional lanes, and current PR
state. Do not replace the existing recipes with a second definition of passing.

The live `main` ruleset (2420853) and `dev` ruleset (13599435), inspected on
2026-09-05 UTC, both require only `Check` and `Test`, with
`strict_required_status_checks_policy: false` and no merge-queue rule in either
ruleset. Refresh effective rules, including any other branch protection, before
changing policy. The classic branch-protection API returned `Branch not protected`
for both branches in the same audit. This is a concrete target-base freshness gap.

- Wrap the existing recipes with structured receipts: head/base/merge-base SHAs,
  source snapshot identity including staged, unstaged, and untracked inputs,
  command, toolchain/lock identities, target/features, exit status, timestamps,
  and artifact paths. Detect changes during a run and invalidate mixed-source
  evidence. Do not include secret file contents in the record.
- Label static inspection, locally built tests, externally supplied binaries,
  CI runs, and hardware runs separately. Record binary digests and provenance
  for `RELAY_BIN`/`MOQ_BIN` overrides; an unknown binary is exploratory evidence.
- Add a read-only PR verification command that fetches current head/base,
  mergeability, required checks, selected extra lanes, and review state. Unknown,
  cancelled, skipped-required, and stale results must not become green.
- Check whether branch policy keeps tests current with the target branch.
  Recommend a merge queue where appropriate, or a strict up-to-date branch
  rule. If a queue is adopted, teach workflows and diff scoping about
  `merge_group`, including the candidate's actual base and head.
- Keep merge execution separate from the report. A report is evidence, not an
  authorization token; a merge action must recheck the expected head and use
  GitHub's enforced gate against concurrent target-branch updates.

Acceptance: change source, an untracked fixture, the PR head, and the target
base after recording a pass. Each must invalidate the affected result. A
missing selected job and a cancelled run block readiness. A docs-only change
still receives a concise valid report without unrelated builds.

[GitHub's merge queue documentation](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
describes testing against the latest target and the required `merge_group`
workflow trigger. Inspect live rules before prescribing a policy change.

## Related

- [PR behavioral gates](/quest/m0/pr-behavioral-gates.md) - owns lane selection and required-result integration
- [Verification preflight](/quest/m0/verification-preflight.md) - supplies environment capability results
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - supplies inspectable runtime evidence
