#!/usr/bin/env bash
# Upload every file in DIR to the R2 bucket BUCKET, keyed by its file name.
#
# Usage: sh/demo/sync.sh BUCKET DIR
set -euo pipefail

usage="usage: sh/demo/sync.sh BUCKET DIR"
bucket=${1:?$usage}
dir=${2:?$usage}

for file in "$dir"/*; do
    [ -e "$file" ] || continue
    key=$(basename "$file")
    echo "Uploading $key..."
    bun wrangler r2 object put "$bucket/$key" --file "$file" --remote
done
