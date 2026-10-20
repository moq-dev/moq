#!/usr/bin/env bash
# Build .oss-scanner/ the way Anthropic's OSS Scanner does: sh/oss-scanner.sh [REF] [REPO]
#
# Runs the scanner's own tools/check from its main branch, so a change on
# either side (a new native dependency here, a new rule there) fails here
# instead of in an email to security@moq.dev. tools/check clones REPO
# anonymously at REF, so REF must be pushed; it defaults to this branch.
set -euo pipefail

ref=${1:-$(git rev-parse --abbrev-ref HEAD)}
repo=${2:-https://github.com/moq-dev/moq}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git clone --quiet --depth 1 https://github.com/anthropics/oss-scanner "$work/oss-scanner"

# The enrolment's project.yaml lives in anthropics/oss-scanner; this mirrors it
# with the ref under test.
mkdir -p "$work/project/moq"
cat >"$work/project/moq/project.yaml" <<EOF
repo: $repo#$ref
primary_contact: security@moq.dev
homepage: https://moq.dev
dockerfile: .oss-scanner/Dockerfile
threat_model: .oss-scanner/threat_model.md
EOF

status=0
uv run --no-project --with pyyaml python3 "$work/oss-scanner/tools/check" --no-shell --dir "$work/check" "$work/project/moq" || status=$?

# tools/check keeps the finished image for its shell; at ~20 GB it would fill the disk.
docker image rm --force oss-scanner-check/built:moq >/dev/null 2>&1 || true
exit "$status"
