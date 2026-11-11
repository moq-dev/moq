#!/usr/bin/env bash
# Submit a new draft version to the IETF datatracker: sh/drafts/publish.sh NAME VERSION EMAIL
#
# The datatracker emails the submitter a confirmation link; the submission is
# not final until that link is clicked. For a brand-new draft (-00) set
# "Replaces" on the confirmation page.
set -euo pipefail

usage="usage: sh/drafts/publish.sh NAME VERSION EMAIL"
name=${1:?$usage}
version=${2:?$usage}
email=${3:?$usage}

cd "$(git rev-parse --show-toplevel)/drafts"
if [[ ! $version =~ ^[0-9][0-9]$ ]]; then
    echo "version must be two digits, e.g. 05" >&2
    exit 1
fi
if [[ ! -f "$name.md" ]]; then
    echo "no such draft: $name.md" >&2
    exit 1
fi

if grep -n ' (in progress)$' "$name.md" >&2; then
    echo "$name.md still has an (in progress) heading; close it before publishing" >&2
    exit 1
fi

doc="$name-$version"
echo "Building $doc.xml"
sed "s/$name-latest/$doc/g" "$name.md" | kramdown-rfc --v3 >"$doc.xml"
echo "Submitting $doc.xml to the datatracker as $email"
curl -sS --fail-with-body -F "user=$email" -F "xml=@$doc.xml" \
    https://datatracker.ietf.org/api/submission
echo
echo "Submitted. Check $email for the confirmation link."
