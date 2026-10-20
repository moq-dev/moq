#!/usr/bin/env bash
# Publish a built relay report (report/build.sh's OUT_DIR) to moq's GitHub
# Pages branch under relays/, and leave that branch checked out in SITE_DIR for
# the Pages deploy.
#
#   report/publish.sh REPORT_DIR SITE_DIR
#
# gh-pages is one site shared with the nightly benchmark charts (bench/). Each
# writer commits only its own directory and deploys the whole branch, so
# neither clobbers the other; the workflows serialize them through the
# `benchmark-pages` concurrency group.
set -euo pipefail

report=$(realpath "$1")
site=$(realpath -m "$2")
cd "$(git rev-parse --show-toplevel)"

sh/gh/bench-publish.sh init
git worktree add "$site" gh-pages
rm -rf "$site/relays"
cp -r "$report" "$site/relays"
touch "$site/.nojekyll"
git -C "$site" add -A relays .nojekyll
if ! git -C "$site" diff --cached --quiet; then
    git -C "$site" -c user.name=github-actions -c user.email=github-actions@users.noreply.github.com \
        commit -m 'Update relay interop report'
fi
git -C "$site" push origin HEAD:gh-pages
