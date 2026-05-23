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
#   APT_SIGNING_KEY           ascii-armored GPG private key (shared with apt repo)
#   APT_SIGNING_KEY_ID        long key id used to pick the signing key
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

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo ">> Sync current repo from R2..."
rclone sync "r2:${BUCKET}/${DIST}" "$WORK/${DIST}" --quiet || mkdir -p "$WORK/${DIST}"

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
    else
        mkdir -p "$WORK/${DIST}/${arch}"
        cp "$rpm" "$WORK/${DIST}/${arch}/"
    fi
done

echo ">> Import signing key..."
GNUPGHOME=$(mktemp -d)
export GNUPGHOME
chmod 700 "$GNUPGHOME"
echo "${APT_SIGNING_KEY:?}" | gpg --batch --quiet --import
KEY_ID="${APT_SIGNING_KEY_ID:?}"

echo ">> Generate repodata per arch..."
for arch in "${ARCHES[@]}"; do
    dir="$WORK/${DIST}/${arch}"
    [[ -d "$dir" ]] || continue
    createrepo_c --update --general-compress-type=gz "$dir"
    gpg --batch --yes --default-key "$KEY_ID" --detach-sign --armor \
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

rm -rf "$GNUPGHOME"

echo ">> Upload to R2..."
rclone sync "$WORK/${DIST}" "r2:${BUCKET}/${DIST}" --quiet
rclone copyto "$WORK/moq.repo" "r2:${BUCKET}/moq.repo" --quiet

echo ">> Done. Repo updated at https://rpm.moq.dev/${DIST}/"
