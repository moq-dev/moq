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
#   SIGNING_KEY               ascii-armored GPG private key (shared with maven publishing)
#   SIGNING_PASSWORD          optional passphrase for SIGNING_KEY
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
GNUPGHOME=""
cleanup() {
    rm -rf "$WORK"
    [[ -n "$GNUPGHOME" ]] && rm -rf "$GNUPGHOME"
}
trap cleanup EXIT

# Pull additively: a partial fetch must never cause subsequent steps to act
# on an incomplete view of the pool, which would drop the missing packages
# from the regenerated Packages indexes.
echo ">> Pull current pool from R2..."
mkdir -p "$WORK/pool"
rclone copy "r2:${BUCKET}/pool" "$WORK/pool" --quiet

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
# GNUPGHOME is removed by the EXIT trap; no need for an explicit `rm -rf`.
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
gpg --batch --yes "${GPG_PASS_ARGS[@]}" --default-key "$KEY_ID" --detach-sign --armor \
    -o "$WORK/dists/$DIST/Release.gpg" \
    "$WORK/dists/$DIST/Release"
gpg --batch --yes "${GPG_PASS_ARGS[@]}" --default-key "$KEY_ID" --clearsign \
    -o "$WORK/dists/$DIST/InRelease" \
    "$WORK/dists/$DIST/Release"

echo ">> Upload pool additions..."
rclone copy "$WORK/pool" "r2:${BUCKET}/pool" --quiet

echo ">> Upload regenerated dists..."
rclone sync "$WORK/dists" "r2:${BUCKET}/dists" --quiet

echo ">> Done. Repo updated at https://apt.moq.dev/dists/$DIST/"
