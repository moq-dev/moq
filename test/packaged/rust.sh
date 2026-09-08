#!/usr/bin/env bash
# Rust half of the packaged-consumer harness. See README.md.
#
# Builds `.crate` archives with the same `cargo package` a release runs, then
# compiles a consumer that lives outside the workspace against nothing but those
# archives. The workspace's own `target/` never reaches the consumer: cargo
# rebuilds every candidate from the extracted archive, so a source file missing
# from the archive is a compile error rather than a silent hit on the build the
# workspace already has.
#
# Called by run.sh, which owns argument parsing and the staging directory.
set -euo pipefail

PACKAGED_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$PACKAGED_DIR/../.." && pwd)
CARGO="${RUST_CARGO:-cargo}"
[[ -n "$CARGO" ]] || CARGO=cargo

# shellcheck source=test/packaged/common.sh
source "$PACKAGED_DIR/common.sh"

STAGE="${1:?usage: rust.sh <stage-dir> <crate>...}"
shift
REQUESTED=("$@")

FEATURES="${PACKAGED_FEATURES:-}"

ARCHIVES="$STAGE/rust/archives"
EXTRACT="$STAGE/rust/extract"
CONSUMER="$STAGE/rust/consumer"
mkdir -p "$ARCHIVES" "$EXTRACT" "$CONSUMER/src"

# ── selection ───────────────────────────────────────────────────────────────
# `cargo metadata --no-deps` describes the workspace only, which is exactly the
# question here: which siblings does a candidate need staged alongside it,
# because they are workspace members whose current version may not exist on
# crates.io yet.
metadata=$("$CARGO" metadata --no-deps --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml")

publishable=$(jq -r '.packages[] | select(.publish == null) | .name' <<<"$metadata" | sort)

# Dev-dependencies are deliberately absent: they never appear in the dependency
# graph a consumer resolves, so staging them would invent blockers a real
# release does not have.
deps_of() {
    jq -r --arg name "$1" '
		.packages[]
		| select(.name == $name)
		| .dependencies[]
		| select(.kind == null or .kind == "build")
		| .name
	' <<<"$metadata"
}

version_of() {
    jq -r --arg name "$1" '.packages[] | select(.name == $name) | .version' <<<"$metadata"
}

manifest_of() {
    jq -r --arg name "$1" '.packages[] | select(.name == $name) | .manifest_path' <<<"$metadata"
}

is_publishable() {
    grep -qx -- "$1" <<<"$publishable"
}

# Candidates, in dependency order: a sibling has to be staged and patched in
# before the crate that depends on it resolves.
candidates=()
seen=""
collect() {
    local name="$1" dep
    grep -qx -- "$name" <<<"$seen" && return 0
    is_publishable "$name" || return 0
    seen=$(printf '%s\n%s' "$seen" "$name")
    for dep in $(deps_of "$name"); do
        collect "$dep"
    done
    candidates+=("$name")
}

for name in ${REQUESTED[@]+"${REQUESTED[@]}"}; do
    if ! is_publishable "$name"; then
        echo "packaged: $name does not publish; skipping" >&2
        continue
    fi
    collect "$name"
done

if ((${#candidates[@]} == 0)); then
    echo "packaged: no publishable Rust crates selected."
    exit 0
fi

# The crates that were asked for, as opposed to the siblings dragged in behind
# them. Only these are reported as verified; a sibling is staged so the build
# can happen at all, and its own archive is only as exercised as this build made it.
requested_set=$(printf '%s\n' ${REQUESTED[@]+"${REQUESTED[@]}"} | sort -u)

echo "packaged: rust candidates: ${candidates[*]}"

# ── manifest audit ──────────────────────────────────────────────────────────
"$PACKAGED_DIR/audit.sh" "$WORKSPACE/Cargo.toml" "${candidates[@]}"

# ── package ─────────────────────────────────────────────────────────────────
# `--no-verify` because cargo's own verification build resolves siblings from
# crates.io, where an unreleased version does not exist. That check is what the
# consumer below replaces, and the consumer is the stronger one: it patches the
# staged siblings in, so it reports what the archive does rather than what the
# registry happens to hold.
#
# `--allow-dirty` because this runs on a working tree, which is the point.
for name in "${candidates[@]}"; do
    version=$(version_of "$name")
    echo "packaged: cargo package -p $name ($version)"
    "$CARGO" package --locked --no-verify --allow-dirty \
        --manifest-path "$(manifest_of "$name")" \
        --package "$name" >/dev/null

    archive="$WORKSPACE/target/package/$name-$version.crate"
    [[ -f "$archive" ]] || {
        echo "packaged: cargo produced no archive at $archive" >&2
        exit 1
    }
    cp "$archive" "$ARCHIVES/"
    tar -xzf "$archive" -C "$EXTRACT"
done

# ── consumer ────────────────────────────────────────────────────────────────
# A binary-only crate cannot be depended on, so it gets consumed the other way
# round: its own archive is built against the patched siblings. Everything with
# a library target goes into one consumer instead.
libs=()
bins=()
for name in "${candidates[@]}"; do
    if [[ "$(jq -r --arg name "$name" '
		[.packages[] | select(.name == $name) | .targets[].kind[]] | index("lib") != null
	' <<<"$metadata")" == true ]]; then
        libs+=("$name")
    else
        bins+=("$name")
    fi
done

# A `[patch.crates-io]` entry per library candidate, so every `moq-*`
# requirement in the graph resolves to the extracted archive instead of the
# registry. This is the explicit-resolution step: nothing we staged can float to
# crates.io, and a name we failed to stage fails loudly at resolution. Only
# libraries appear here; cargo warns about a patch nothing can depend on.
patch_table() {
    echo "[patch.crates-io]"
    for name in ${libs[@]+"${libs[@]}"}; do
        echo "$name = { path = \"$EXTRACT/$name-$(version_of "$name")\" }"
    done
}

feature_args=()
[[ -z "$FEATURES" ]] || feature_args=(--features "$FEATURES")

if ((${#libs[@]} > 0)); then
    {
        cat "$PACKAGED_DIR/rust/consumer.toml"
        echo
        echo "[dependencies]"
        for name in "${libs[@]}"; do
            echo "$name = \"=$(version_of "$name")\""
        done
        echo
        patch_table
    } >"$CONSUMER/Cargo.toml"

    # The consumer body: a released-API fixture per crate that has one, and a bare
    # link for the rest. The bare link still compiles the whole archive, which is
    # what catches a missing file; the fixtures add the part semver tooling cannot
    # see, which is whether the API a consumer already wrote still means the same
    # thing.
    {
        echo "//! Generated by test/packaged/rust.sh. See test/packaged/README.md."
        for name in "${libs[@]}"; do
            crate="${name//-/_}"
            fixture="$PACKAGED_DIR/rust/api/$name.rs"
            if [[ -f "$fixture" ]]; then
                echo
                echo "// test/packaged/rust/api/$name.rs"
                echo "pub mod ${crate}_api {"
                sed 's/^/    /' "$fixture"
                echo "}"
            else
                echo "use $crate as _;"
            fi
        done
    } >"$CONSUMER/src/lib.rs"

    echo "packaged: building the isolated consumer"
    (
        # `cd` rather than `--manifest-path`: cargo discovers config and workspace
        # roots by walking up from the current directory, and the whole point is to
        # stand outside the repo's.
        cd "$CONSUMER"
        "$CARGO" build ${feature_args[@]+"${feature_args[@]}"}
    )
fi

if ((${#bins[@]} > 0)); then
    # Built from a copy of the extraction, in a throwaway workspace that owns the
    # patch table: the archive itself stays exactly as cargo produced it.
    root="$STAGE/rust/bins"
    mkdir -p "$root"
    {
        echo "[workspace]"
        echo "resolver = \"3\""
        echo "members = ["
        for name in "${bins[@]}"; do
            echo "    \"$name\","
        done
        echo "]"
        echo
        patch_table
    } >"$root/Cargo.toml"
    for name in "${bins[@]}"; do
        cp -R "$EXTRACT/$name-$(version_of "$name")" "$root/$name"
    done

    echo "packaged: building the binary archives: ${bins[*]}"
    (cd "$root" && "$CARGO" build ${feature_args[@]+"${feature_args[@]}"})
fi

# ── report ──────────────────────────────────────────────────────────────────
echo
echo "packaged: rust archives"
for name in "${candidates[@]}"; do
    version=$(version_of "$name")
    file="$ARCHIVES/$name-$version.crate"
    role="staged sibling"
    grep -qx -- "$name" <<<"$requested_set" && role="verified"
    printf '  %-20s %-10s %s  %s\n' "$name" "$version" "$(sha256_of "$file")" "$role"
done
echo
echo "packaged: consumer manifest $CONSUMER/Cargo.toml"
echo "packaged: consumer command  cd $CONSUMER && $CARGO build ${feature_args[*]-}"
