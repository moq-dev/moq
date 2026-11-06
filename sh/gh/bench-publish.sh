#!/usr/bin/env bash
set -euo pipefail

case ${1:-} in
    init)
        # ls-remote distinguishes an absent history branch from an authentication failure.
        if git ls-remote --exit-code --heads origin gh-pages; then
            git fetch origin gh-pages:gh-pages
        else
            status=$?
            if [[ $status != 2 ]]; then exit "$status"; fi
            tree=$(git hash-object -t tree /dev/null)
            commit=$(git -c user.name=github-actions -c user.email=github-actions@users.noreply.github.com \
                commit-tree "$tree" -m 'Initialize benchmark history')
            git branch gh-pages "$commit"
        fi
        ;;
    finish)
        if [[ $# != 3 ]]; then
            echo 'usage: benchmark-publish finish DIRECTORY PAIRED' >&2
            exit 2
        fi
        paired=$(realpath "$3")
        git worktree add "$2" gh-pages
        # The checkout is the nightly's commit; a source without a point for it failed tonight.
        just gh benchmark-report index "$2" --commit "$(git rev-parse HEAD)" --paired "$paired"
        git -C "$2" add index.html .nojekyll bench
        if ! git -C "$2" diff --cached --quiet; then
            git -C "$2" -c user.name=github-actions -c user.email=github-actions@users.noreply.github.com \
                commit -m 'Update benchmark table'
        fi
        git -C "$2" push origin HEAD:gh-pages
        ;;
    *)
        echo 'usage: benchmark-publish {init|finish DIRECTORY PAIRED}' >&2
        exit 2
        ;;
esac
