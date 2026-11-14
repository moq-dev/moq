# shellcheck shell=bash
# Sourced by the go.moq.dev scripts: sets BUCKET, the rclone destination, from
# GO_BUCKET (default r2:go-moq-dev), and configures the r2: remote when used.
#
# Required environment, unless GO_BUCKET names a non-R2 rclone path:
#   R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY - R2 token with object write on go-moq-dev

BUCKET="${GO_BUCKET:-r2:go-moq-dev}"

if [[ "$BUCKET" == r2:* ]]; then
    export RCLONE_CONFIG_R2_TYPE=s3
    export RCLONE_CONFIG_R2_PROVIDER=Cloudflare
    export RCLONE_CONFIG_R2_ENDPOINT=https://dd618f5dbd5da77b8296f1613c301f5c.r2.cloudflarestorage.com
    export RCLONE_CONFIG_R2_ACCESS_KEY_ID="${R2_ACCESS_KEY_ID:?R2_ACCESS_KEY_ID is required}"
    export RCLONE_CONFIG_R2_SECRET_ACCESS_KEY="${R2_SECRET_ACCESS_KEY:?R2_SECRET_ACCESS_KEY is required}"
    # The token can write objects but not create buckets, which rclone tries first.
    export RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
fi
