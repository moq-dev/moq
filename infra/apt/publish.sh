#!/usr/bin/env bash
#
# Regenerate apt repo metadata and push to the apt-moq-dev R2 bucket.
# Pull the current pool, merge in new .deb files from $ARTIFACTS_DIR,
# rebuild dists/stable metadata with apt-ftparchive, sign with GPG, and
# upload only what changed.
#
# Required env:
#   ARTIFACTS_DIR             directory containing new .deb files to add
#   R2_ACCESS_KEY_ID          R2 API token
#   R2_SECRET_ACCESS_KEY
#   R2_ACCOUNT_ID
#   APT_SIGNING_KEY           ascii-armored GPG private key
#   APT_SIGNING_KEY_ID        long key id used to pick the signing key
#
# Required tools: rclone, apt-ftparchive (apt-utils), gpg, dpkg-scanpackages.

set -euo pipefail

ARTIFACTS_DIR="${ARTIFACTS_DIR:-artifacts}"
BUCKET="apt-moq-dev"
DIST="stable"
COMPONENT="main"
ORIGIN="MoQ Project"
LABEL="moq"
SUITE="$DIST"
DESCRIPTION="Media over QUIC apt repository"
ARCHES=(amd64 arm64)

# Make rclone talk to R2. R2 is S3-compatible.
export RCLONE_CONFIG_R2_TYPE=s3
export RCLONE_CONFIG_R2_PROVIDER=Cloudflare
export RCLONE_CONFIG_R2_ENDPOINT="https://${R2_ACCOUNT_ID:?}.r2.cloudflarestorage.com"
export RCLONE_CONFIG_R2_ACCESS_KEY_ID="${R2_ACCESS_KEY_ID:?}"
export RCLONE_CONFIG_R2_SECRET_ACCESS_KEY="${R2_SECRET_ACCESS_KEY:?}"
export RCLONE_CONFIG_R2_ACL=private

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo ">> Sync current pool from R2..."
rclone sync "r2:${BUCKET}/pool" "$WORK/pool" --quiet || mkdir -p "$WORK/pool"

echo ">> Add new .deb files to pool..."
shopt -s nullglob
new_debs=("$ARTIFACTS_DIR"/*.deb)
if [[ ${#new_debs[@]} -eq 0 ]]; then
    echo "No .deb files in $ARTIFACTS_DIR; nothing to do." >&2
    exit 0
fi
for deb in "${new_debs[@]}"; do
    pkg=$(dpkg-deb -f "$deb" Package)
    dest="$WORK/pool/main/${pkg:0:1}/${pkg}"
    mkdir -p "$dest"
    cp "$deb" "$dest/"
done

echo ">> Generate Packages indexes per arch..."
mkdir -p "$WORK/dists/$DIST/$COMPONENT"
for arch in "${ARCHES[@]}"; do
    out="$WORK/dists/$DIST/$COMPONENT/binary-${arch}"
    mkdir -p "$out"
    (cd "$WORK" && apt-ftparchive --arch "$arch" packages "pool/$COMPONENT") \
        > "$out/Packages"
    gzip -9kf "$out/Packages"
done

echo ">> Generate Release..."
cat > "$WORK/apt-ftparchive.conf" <<EOF
APT::FTPArchive::Release::Origin "$ORIGIN";
APT::FTPArchive::Release::Label "$LABEL";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$DIST";
APT::FTPArchive::Release::Architectures "${ARCHES[*]}";
APT::FTPArchive::Release::Components "$COMPONENT";
APT::FTPArchive::Release::Description "$DESCRIPTION";
EOF
(cd "$WORK" && apt-ftparchive -c=apt-ftparchive.conf release "dists/$DIST") \
    > "$WORK/dists/$DIST/Release"

echo ">> Sign Release..."
GNUPGHOME=$(mktemp -d)
export GNUPGHOME
chmod 700 "$GNUPGHOME"
echo "${APT_SIGNING_KEY:?}" | gpg --batch --quiet --import
KEY_ID="${APT_SIGNING_KEY_ID:?}"
gpg --batch --yes --default-key "$KEY_ID" --detach-sign --armor \
    -o "$WORK/dists/$DIST/Release.gpg" \
    "$WORK/dists/$DIST/Release"
gpg --batch --yes --default-key "$KEY_ID" --clearsign \
    -o "$WORK/dists/$DIST/InRelease" \
    "$WORK/dists/$DIST/Release"
rm -rf "$GNUPGHOME"

echo ">> Upload pool additions..."
rclone copy "$WORK/pool" "r2:${BUCKET}/pool" --quiet

echo ">> Upload regenerated dists..."
rclone sync "$WORK/dists" "r2:${BUCKET}/dists" --quiet

echo ">> Done. Repo updated at https://apt.moq.dev/dists/$DIST/"
