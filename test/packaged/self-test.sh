#!/usr/bin/env bash
# Negative controls for the packaged-consumer harness.
#
# A harness that only ever passes is indistinguishable from one that checks
# nothing, and this one is easy to break silently: a consumer that quietly
# resolved a package from the registry, or a `cargo build` that hit the
# workspace's target/ instead of the extracted archive, both look like a pass.
#
# So break a candidate on purpose, three ways, and require the matching check to
# fail. Every fixture is built inside the staging directory from a copy, so
# nothing here touches the checkout.
#
#     just test packaged --self-test
set -euo pipefail

PACKAGED_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$PACKAGED_DIR/../.." && pwd)
CARGO="${RUST_CARGO:-cargo}"
[[ -n "$CARGO" ]] || CARGO=cargo

# shellcheck source=test/packaged/common.sh
source "$PACKAGED_DIR/common.sh"

STAGE="${1:?usage: self-test.sh <stage-dir>}"

# kio and @moq/net are the smallest candidates that still have the shape each
# fixture needs: a module tree the crate root declares unconditionally, and a
# runtime dependency the entry point imports.
RUST_CRATE=kio
JS_PACKAGE=@moq/net

pass=0
fail=0
check() {
    local name="$1"
    shift
    if "$@"; then
        echo "  ok   $name"
        pass=$((pass + 1))
    else
        echo "  FAIL $name" >&2
        fail=$((fail + 1))
    fi
}

# ── a path dependency with no publishable version ───────────────────────────
# Two crates in a disposable workspace, the publishable one depending on the
# other by path alone. Cargo resolves that fine in the workspace and cannot
# publish it at all.
missing_version() {
    local root="$STAGE/self-test/no-version"
    mkdir -p "$root/publishable/src" "$root/sibling/src"
    cat >"$root/Cargo.toml" <<-'EOF'
		[workspace]
		members = ["publishable", "sibling"]
		resolver = "3"
	EOF
    cat >"$root/publishable/Cargo.toml" <<-'EOF'
		[package]
		name = "publishable"
		version = "0.1.0"
		edition = "2024"

		[dependencies]
		sibling = { path = "../sibling" }
	EOF
    cat >"$root/sibling/Cargo.toml" <<-'EOF'
		[package]
		name = "sibling"
		version = "0.1.0"
		edition = "2024"
	EOF
    touch "$root/publishable/src/lib.rs" "$root/sibling/src/lib.rs"

    # The audit must refuse it, and it must accept the same workspace once the
    # version is there: a check that fails on everything is not a check.
    if "$PACKAGED_DIR/audit.sh" "$root/Cargo.toml" publishable 2>/dev/null; then
        echo "    the audit accepted a path dependency with no version" >&2
        return 1
    fi
    sed -i.bak 's|{ path = "../sibling" }|{ path = "../sibling", version = "0.1.0" }|' \
        "$root/publishable/Cargo.toml"
    if ! "$PACKAGED_DIR/audit.sh" "$root/Cargo.toml" publishable; then
        echo "    the audit refused a versioned path dependency" >&2
        return 1
    fi
}

# ── a required file missing from the archive ────────────────────────────────
# The defect an `exclude` typo or a stale `include` list produces: the workspace
# still has the file, so only a build from the extracted archive notices.
missing_archive_file() {
    local root="$STAGE/self-test/missing-file"
    local version extract
    version=$("$CARGO" metadata --no-deps --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml" |
        jq -r --arg n "$RUST_CRATE" '.packages[] | select(.name == $n) | .version')

    mkdir -p "$root/extract" "$root/consumer/src"
    "$CARGO" package --locked --no-verify --allow-dirty --package "$RUST_CRATE" >/dev/null
    tar -xzf "$WORKSPACE/target/package/$RUST_CRATE-$version.crate" -C "$root/extract"
    extract="$root/extract/$RUST_CRATE-$version"

    {
        cat "$PACKAGED_DIR/rust/consumer.toml"
        echo
        echo "[dependencies]"
        echo "$RUST_CRATE = \"=$version\""
        echo
        echo "[patch.crates-io]"
        echo "$RUST_CRATE = { path = \"$extract\" }"
    } >"$root/consumer/Cargo.toml"
    echo "use ${RUST_CRATE//-/_} as _;" >"$root/consumer/src/lib.rs"

    # Baseline: the intact archive has to build, or the fixture below proves nothing.
    if ! (cd "$root/consumer" && "$CARGO" build --quiet 2>&1 | sed 's/^/    /'); then
        echo "    the intact $RUST_CRATE archive did not build" >&2
        return 1
    fi

    # A module the crate root declares unconditionally, so removing it breaks a
    # default-feature build. Read out of lib.rs rather than listed here, so this
    # keeps working as the crate is reorganized; the `#[cfg(...)]` ones are
    # skipped because a default build never reads them.
    local victim
    victim=$(awk '
		/^#\[/ { gated = 1; next }
		/^(pub )?mod [a-z_]+;/ { if (!gated) { gsub(/^(pub )?mod |;/, ""); print; exit } }
		{ gated = 0 }
	' "$extract/src/lib.rs")
    [[ -n "$victim" && -f "$extract/src/$victim.rs" ]] || {
        echo "    $RUST_CRATE declares no unconditional module to remove" >&2
        return 1
    }
    rm "$extract/src/$victim.rs"
    if (cd "$root/consumer" && "$CARGO" build --quiet >/dev/null 2>&1); then
        echo "    the consumer built with src/$victim.rs missing from the archive" >&2
        return 1
    fi
}

# ── an imported dependency the manifest omits ───────────────────────────────
# The defect a workspace install cannot have: every `@moq/*` package resolves
# through the same hoisted node_modules there, declared or not.
missing_js_dependency() {
    local root="$STAGE/self-test/missing-dep"
    local dir dep tarball
    dir=$(jq -r '.workspaces[]' "$WORKSPACE/package.json" | while read -r d; do
        [[ -f "$WORKSPACE/$d/package.json" ]] || continue
        [[ "$(jq -r .name "$WORKSPACE/$d/package.json")" == "$JS_PACKAGE" ]] && echo "$d"
    done)
    [[ -n "$dir" ]] || {
        echo "    $JS_PACKAGE is not a workspace" >&2
        return 1
    }

    mkdir -p "$root/pkg" "$root/consumer"
    (cd "$WORKSPACE/$dir" && bun run build >/dev/null)
    cp -R "$WORKSPACE/$dir/dist/." "$root/pkg/"

    # A dependency the entry point actually imports, so dropping it breaks
    # resolution rather than going unnoticed.
    dep=$(jq -r '.dependencies | keys[]' "$root/pkg/package.json" |
        while read -r candidate; do
            grep -qr "\"$candidate\"" "$root/pkg"/*.js 2>/dev/null && echo "$candidate"
        done | head -1)
    [[ -n "$dep" ]] || {
        echo "    no imported dependency found in the $JS_PACKAGE package" >&2
        return 1
    }

    jq --arg dep "$dep" 'del(.dependencies[$dep])' "$root/pkg/package.json" >"$root/pkg/package.json.next"
    mv "$root/pkg/package.json.next" "$root/pkg/package.json"
    tarball=$(cd "$root/pkg" && npm pack --silent --pack-destination "$root")

    cat >"$root/consumer/package.json" <<-EOF
		{
		  "name": "packaged-consumer-self-test",
		  "private": true,
		  "type": "module",
		  "dependencies": { "$JS_PACKAGE": "file:$root/$tarball" }
		}
	EOF
    cp "$PACKAGED_DIR/js/imports.mjs" "$root/consumer/"
    (cd "$root/consumer" && npm install --no-audit --no-fund >/dev/null 2>&1)

    if (cd "$root/consumer" && node imports.mjs "$JS_PACKAGE" >/dev/null 2>&1); then
        echo "    $JS_PACKAGE imported cleanly without declaring $dep" >&2
        return 1
    fi
}

echo "packaged: negative controls"
check "a path dependency with no version is refused" missing_version
check "a file missing from the archive fails the consumer build" missing_archive_file
check "an undeclared JS dependency fails the entry-point import" missing_js_dependency

echo
echo "packaged: $pass passed, $fail failed"
((fail == 0))
