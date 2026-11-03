#!/usr/bin/env bash
# Publish a built report to moq's GitHub Pages branch under DIRECTORY/, and leave
# that branch checked out in SITE for the Pages deploy.
#
#   sh/gh/pages-publish.sh DIRECTORY REPORT_DIR SITE_DIR
#
# gh-pages is one site shared by every nightly report: the benchmark table
# (bench/ and the root index), the relay report (relays/), and the interop report
# (interop/). Each writer commits only its own directory and deploys the whole
# branch, so none clobbers another; the workflows serialize them through the
# `benchmark-pages` concurrency group.
set -euo pipefail

if [[ $# != 3 || ! "$1" =~ ^[a-z][a-z0-9-]*$ ]]; then
    echo 'usage: pages-publish DIRECTORY REPORT_DIR SITE_DIR (DIRECTORY is one lowercase path segment)' >&2
    exit 2
fi
directory=$1
report=$(realpath "$2")
site=$(realpath -m "$3")
cd "$(git rev-parse --show-toplevel)"

sh/gh/bench-publish.sh init
git worktree add "$site" gh-pages
rm -rf "${site:?}/$directory"
cp -r "$report" "$site/$directory"
touch "$site/.nojekyll"
git -C "$site" add -A "$directory" .nojekyll
if ! git -C "$site" diff --cached --quiet; then
    git -C "$site" -c user.name=github-actions -c user.email=github-actions@users.noreply.github.com \
        commit -m "Update $directory report"
fi
git -C "$site" push origin HEAD:gh-pages
