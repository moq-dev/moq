#!/usr/bin/env bash
#
# Open (or refresh) the pull request that merges `release` back into `main`.
# Check's `land` job merges it as a merge commit once Check and Test pass on its
# head, bypassing the squash-only merge queue.
#
# The pull request's head is its own branch rather than `release` itself: the
# repository deletes head branches on merge, and a conflict is resolved by
# merging `main` into that branch, which must never touch `release`.
#
# Required env:
#   GH_TOKEN           A token whose pull requests trigger CI (not GITHUB_TOKEN).
#   GITHUB_REPOSITORY  owner/repo, set by GitHub Actions.

set -euo pipefail

repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be set}"
branch=merge/release-into-main

ahead=$(gh api "repos/$repo/compare/main...release" --jq .ahead_by)
if [[ "$ahead" == 0 ]]; then
    echo "main already contains release"
    exit 0
fi

sha=$(gh api "repos/$repo/git/ref/heads/release" --jq .object.sha)

# --exit-code: 2 means the branch is absent; anything else nonzero is a failure.
status=0
git ls-remote --exit-code --heads "https://github.com/$repo" "$branch" >/dev/null || status=$?
case "$status" in
    0)
        # An open pull request may carry a hand-resolved conflict, so merge into the
        # branch instead of resetting it. A conflict fails here, loudly.
        gh api -X POST "repos/$repo/merges" -f base="$branch" -f head="$sha" \
            -f commit_message="Merge release into $branch" >/dev/null
        ;;
    2)
        gh api -X POST "repos/$repo/git/refs" -f ref="refs/heads/$branch" -f sha="$sha" >/dev/null
        ;;
    *)
        echo "error: cannot query $branch on $repo" >&2
        exit 1
        ;;
esac

# Only a pull request from this repository's own branch: `gh pr list --head`
# matches the branch name alone, so a fork's branch of the same name would pass.
pr=$(gh api "repos/$repo/pulls?state=open&base=main&head=${repo%%/*}:$branch" \
    --jq "[.[] | select(.head.repo.full_name == \"$repo\")][0].number // empty")
if [[ -z "$pr" ]]; then
    gh pr create --repo "$repo" --head "$branch" --base main \
        --title "chore: merge release into main" \
        --body "Carries what \`release\` published (versions, CHANGELOGs, backports) back to trunk. Opened by the Back-merge workflow.

Check's \`land\` job merges it as a merge commit once Check and Test pass. **Never squash.** A squash leaves the merge base at the last cut, so the next back-merge conflicts. On a conflict, merge \`main\` into \`$branch\` and resolve it there."
fi
