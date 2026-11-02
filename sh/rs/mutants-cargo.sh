#!/usr/bin/env bash
# The `$CARGO` cargo-mutants calls. It names a package `name@version`, which is
# ambiguous for moq-auth, since its tests depend on its own published release;
# a path spec names the workspace member alone.
set -euo pipefail

args=()
for arg in "$@"; do
    if [[ $arg =~ ^--package=([A-Za-z0-9_-]+)@ && -f rs/${BASH_REMATCH[1]}/Cargo.toml ]]; then
        arg="--package=path+file://$PWD/rs/${BASH_REMATCH[1]}"
    fi
    args+=("$arg")
done
exec cargo "${args[@]}"
