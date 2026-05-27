#!/usr/bin/env bash
#
# Regenerate yum/dnf repo metadata and push to the rpm-moq-dev R2 bucket.
# Pull the current pool, merge in new .rpm files from $ARTIFACTS_DIR,
# rebuild repodata with createrepo_c, sign repomd.xml with GPG, and upload.
#
# Required env:
#   ARTIFACTS_DIR             directory containing new .rpm files to add
#   R2_ACCESS_KEY_ID          R2 API token
#   R2_SECRET_ACCESS_KEY
#   R2_ACCOUNT_ID
#   SIGNING_KEY               ascii-armored GPG private key (shared with apt repo and maven publishing)
#   SIGNING_PASSWORD          optional passphrase for SIGNING_KEY
#
# Required tools: rclone, createrepo_c, gpg.

set -euo pipefail

ARTIFACTS_DIR="${ARTIFACTS_DIR:-artifacts}"
BUCKET="rpm-moq-dev"
DIST="el9"
ARCHES=(x86_64 aarch64)

# Make rclone talk to R2.
export RCLONE_CONFIG_R2_TYPE=s3
export RCLONE_CONFIG_R2_PROVIDER=Cloudflare
export RCLONE_CONFIG_R2_ENDPOINT="https://${R2_ACCOUNT_ID:?}.r2.cloudflarestorage.com"
export RCLONE_CONFIG_R2_ACCESS_KEY_ID="${R2_ACCESS_KEY_ID:?}"
export RCLONE_CONFIG_R2_SECRET_ACCESS_KEY="${R2_SECRET_ACCESS_KEY:?}"
export RCLONE_CONFIG_R2_ACL=private
# The R2 token has object read/write but not CreateBucket. rclone normally
# probes the bucket on writes to a bucket-root key (e.g. moq.repo), which
# surfaces as a 403 AccessDenied. Skip the probe; the bucket already exists.
export RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true

WORK=$(mktemp -d)
GNUPGHOME=""
cleanup() {
    rm -rf "$WORK"
    [[ -n "$GNUPGHOME" ]] && rm -rf "$GNUPGHOME"
}
trap cleanup EXIT

# Pull additively: a partial fetch must never cause the push step to delete
# remote .rpms. createrepo_c --update overwrites repodata in place, so a
# stale local repodata is fine - the regenerate step rewrites it.
echo ">> Pull current repo from R2..."
mkdir -p "$WORK/${DIST}"
rclone copy "r2:${BUCKET}/${DIST}" "$WORK/${DIST}" --quiet

echo ">> Sort new .rpm files by arch..."
shopt -s nullglob
new_rpms=("$ARTIFACTS_DIR"/*.rpm)
if [[ ${#new_rpms[@]} -eq 0 ]]; then
    echo "No .rpm files in $ARTIFACTS_DIR; nothing to do." >&2
    exit 0
fi
for rpm in "${new_rpms[@]}"; do
    arch=$(rpm -qp --queryformat '%{ARCH}' "$rpm")
    # Map noarch packages into every supported per-arch tree.
    if [[ "$arch" == "noarch" ]]; then
        for a in "${ARCHES[@]}"; do
            mkdir -p "$WORK/${DIST}/${a}"
            cp "$rpm" "$WORK/${DIST}/${a}/"
        done
    elif [[ " ${ARCHES[*]} " == *" ${arch} "* ]]; then
        mkdir -p "$WORK/${DIST}/${arch}"
        cp "$rpm" "$WORK/${DIST}/${arch}/"
    else
        echo "ERROR: ${rpm} has unsupported arch '${arch}'; expected one of: ${ARCHES[*]} or noarch." >&2
        exit 1
    fi
done

echo ">> Import signing key..."
GNUPGHOME=$(mktemp -d)
export GNUPGHOME
chmod 700 "$GNUPGHOME"
echo "${SIGNING_KEY:?}" | gpg --batch --quiet --import
# Fail loud if SIGNING_KEY ever holds more than one secret. Silently picking
# the first one would produce signatures from the wrong key.
mapfile -t KEY_IDS < <(gpg --list-secret-keys --with-colons --keyid-format=long \
    | awk -F: '/^sec:/ { print $5 }')
if [[ ${#KEY_IDS[@]} -ne 1 ]]; then
    echo "ERROR: expected exactly one secret key in SIGNING_KEY, found ${#KEY_IDS[@]}." >&2
    exit 1
fi
KEY_ID="${KEY_IDS[0]}"
GPG_PASS_ARGS=()
if [[ -n "${SIGNING_PASSWORD:-}" ]]; then
    GPG_PASS_ARGS=(--pinentry-mode loopback --passphrase "$SIGNING_PASSWORD")
fi

echo ">> Generate repodata per arch..."
for arch in "${ARCHES[@]}"; do
    dir="$WORK/${DIST}/${arch}"
    [[ -d "$dir" ]] || continue
    createrepo_c --update --general-compress-type=gz "$dir"
    gpg --batch --yes "${GPG_PASS_ARGS[@]}" --default-key "$KEY_ID" --detach-sign --armor \
        -o "$dir/repodata/repomd.xml.asc" \
        "$dir/repodata/repomd.xml"
done

echo ">> Write moq.repo template..."
cat > "$WORK/moq.repo" <<EOF
[moq]
name=MoQ Project
baseurl=https://rpm.moq.dev/${DIST}/\$basearch
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://rpm.moq.dev/moq-archive-keyring.gpg
EOF

# GNUPGHOME is removed by the EXIT trap; no need for an explicit `rm -rf`.

# Push .rpm blobs additively, but sync repodata so old indices are replaced.
# Mixing the two avoids ever deleting an .rpm that survived a partial pull.
echo ">> Upload to R2..."
for arch in "${ARCHES[@]}"; do
    dir="$WORK/${DIST}/${arch}"
    [[ -d "$dir" ]] || continue
    rclone copy "$dir" "r2:${BUCKET}/${DIST}/${arch}" --include "*.rpm" --quiet
    rclone sync "$dir/repodata" "r2:${BUCKET}/${DIST}/${arch}/repodata" --quiet
done
rclone copyto "$WORK/moq.repo" "r2:${BUCKET}/moq.repo" --quiet

echo ">> Done. Repo updated at https://rpm.moq.dev/${DIST}/"
