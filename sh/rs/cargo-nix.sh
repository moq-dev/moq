#!/usr/bin/env bash
# Regenerate nix/Cargo.nix, the per-crate Nix build plan (nix/crates.nix), from
# Cargo.lock and the workspace manifests. With --check, fail instead when the
# committed plan is stale.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

check=
case "${1:-}" in
    --check) check=1 ;;
    "") ;;
    *)
        echo "usage: $0 [--check]" >&2
        exit 2
        ;;
esac

crate2nix="$(nix build --no-link --print-out-paths --inputs-from . nixpkgs#crate2nix)/bin/crate2nix"

plan=nix/Cargo.nix
hashes=nix/crate-hashes.json
if [[ -n "$check" ]]; then
    # One directory deep, as nix/ is, so its relative crate paths match, and
    # ignored, so no parallel lane's formatter lists it.
    mkdir -p .tmp
    plan=$(mktemp .tmp/Cargo.XXXXXX.nix)
    hashes=$(mktemp .tmp/crate-hashes.XXXXXX.json)
    trap 'rm -f "$plan" "$hashes"' EXIT
    cp nix/crate-hashes.json "$hashes"
fi

# Quiet unless it fails: it warns about every crate the uniffi fork shadows.
if ! log=$("$crate2nix" generate -f Cargo.toml -o "$plan" -h "$hashes" 2>&1); then
    printf '%s\n' "$log" >&2
    exit 1
fi
nixfmt "$plan"

if [[ -n "$check" ]]; then
    # The header records the command, which names the scratch paths.
    sed -i "2s|$plan|nix/Cargo.nix|; 2s|$hashes|nix/crate-hashes.json|" "$plan"
    if ! diff -q nix/Cargo.nix "$plan" >/dev/null || ! diff -q nix/crate-hashes.json "$hashes" >/dev/null; then
        echo "error: nix/Cargo.nix is stale for Cargo.lock and the manifests; run \`just rs cargo-nix\`" >&2
        exit 1
    fi
fi
