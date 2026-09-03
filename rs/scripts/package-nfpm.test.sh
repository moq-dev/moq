#!/usr/bin/env bash
set -euo pipefail

if ! command -v envsubst >/dev/null 2>&1; then
    exit 0
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/capture"

cat >"$scratch/bin/nfpm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

while (($#)); do
	case "$1" in
		--config)
			config=$2
			shift 2
			;;
		--packager)
			printf '%s\n' "$2" >"$NFPM_CAPTURE/packager"
			shift 2
			;;
		--target)
			printf '%s\n' "$2" >"$NFPM_CAPTURE/target"
			shift 2
			;;
		*) shift ;;
	esac
done

cp "$config" "$NFPM_CAPTURE/config.yaml"
SH
chmod +x "$scratch/bin/nfpm"

cat >"$scratch/nfpm.yaml" <<'YAML'
name: test
version: ${VERSION}
arch: ${ARCH}
contents:
  - src: ${BINARY_PATH}
    dst: /usr/bin/test
YAML

PATH="$scratch/bin:$PATH" \
    NFPM_CAPTURE="$scratch/capture" \
    VERSION=1.2.3 \
    ARCH=amd64 \
    BINARY_PATH=target/release/test \
    rs/scripts/package-nfpm.sh "$scratch/nfpm.yaml" deb "$scratch/dist"

grep -qx 'version: 1.2.3' "$scratch/capture/config.yaml"
grep -qx 'arch: amd64' "$scratch/capture/config.yaml"
grep -qx '  - src: target/release/test' "$scratch/capture/config.yaml"
grep -qx 'deb' "$scratch/capture/packager"
grep -qx "$scratch/dist" "$scratch/capture/target"

if PATH="$scratch/bin:$PATH" \
    NFPM_CAPTURE="$scratch/capture" \
    VERSION=1.2.3 \
    ARCH=amd64 \
    rs/scripts/package-nfpm.sh "$scratch/nfpm.yaml" deb "$scratch/dist" 2>"$scratch/missing.log"; then
    echo "package-nfpm accepted a missing BINARY_PATH" >&2
    exit 1
fi
grep -qx "Missing environment variable in $scratch/nfpm.yaml: BINARY_PATH" "$scratch/missing.log"
