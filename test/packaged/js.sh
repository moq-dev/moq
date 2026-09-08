#!/usr/bin/env bash
# JavaScript half of the packaged-consumer harness. See README.md.
#
# `bun run build` already rewrites each package.json for publication and runs
# publint over the result, so the manifest is linted before anything here. What
# it cannot say is whether the tarball works once the bun workspace is gone: in
# the workspace every `@moq/*` import resolves through a symlink into a sibling
# source tree, whether or not the importing package declares the dependency and
# whether or not the file is in the published `files` set.
#
# So: pack the built dist, install the tarballs into a consumer outside the
# workspace, and resolve every documented entry point there. node_modules in
# that consumer holds only what the tarballs declare.
#
# Called by run.sh, which owns argument parsing and the staging directory.
set -euo pipefail

PACKAGED_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$PACKAGED_DIR/../.." && pwd)

# shellcheck source=test/packaged/common.sh
source "$PACKAGED_DIR/common.sh"

STAGE="${1:?usage: js.sh <stage-dir> <package>...}"
shift
REQUESTED=("$@")

ARCHIVES="$STAGE/js/archives"
CONSUMER="$STAGE/js/consumer"
mkdir -p "$ARCHIVES"

# ── selection ───────────────────────────────────────────────────────────────
# The bun workspace list is the repo-root package.json, and a workspace entry
# publishes exactly when it has a `release` script (the same predicate
# js/common/package.ts uses to decide whether to emit a jsr.json).
workspaces=$(jq -r '.workspaces[]' "$WORKSPACE/package.json")

dir_of() {
    local name="$1" dir
    for dir in $workspaces; do
        [[ -f "$WORKSPACE/$dir/package.json" ]] || continue
        if [[ "$(jq -r .name "$WORKSPACE/$dir/package.json")" == "$name" ]]; then
            echo "$dir"
            return 0
        fi
    done
    return 1
}

publishes() {
    local dir
    dir=$(dir_of "$1") || return 1
    [[ "$(jq -r '.scripts.release // ""' "$WORKSPACE/$dir/package.json")" != "" ]]
}

# Workspace dependencies, which is what "unpublished sibling" means here: a
# `workspace:` range is rewritten to `^<current version>` at build time, and that
# version may not be on npm yet.
deps_of() {
    local dir
    dir=$(dir_of "$1") || return 0
    jq -r '
		((.dependencies // {}) + (.peerDependencies // {}))
		| to_entries[]
		| select(.value | startswith("workspace:"))
		| .key
	' "$WORKSPACE/$dir/package.json"
}

candidates=()
seen=""
collect() {
    local name="$1" dep
    grep -qx -- "$name" <<<"$seen" && return 0
    publishes "$name" || return 0
    seen=$(printf '%s\n%s' "$seen" "$name")
    for dep in $(deps_of "$name"); do
        collect "$dep"
    done
    candidates+=("$name")
}

for name in ${REQUESTED[@]+"${REQUESTED[@]}"}; do
    if ! publishes "$name"; then
        echo "packaged: $name does not publish; skipping" >&2
        continue
    fi
    collect "$name"
done

if ((${#candidates[@]} == 0)); then
    echo "packaged: no publishable JS packages selected."
    exit 0
fi

requested_set=$(printf '%s\n' ${REQUESTED[@]+"${REQUESTED[@]}"} | sort -u)
echo "packaged: js candidates: ${candidates[*]}"

# ── pack ────────────────────────────────────────────────────────────────────
declare -a tarballs=()
for name in "${candidates[@]}"; do
    dir=$(dir_of "$name")
    echo "packaged: bun run build ($name)"
    (cd "$WORKSPACE/$dir" && bun run build >/dev/null)

    # `npm pack` from dist/, not from the package root: dist/package.json is the
    # manifest that gets published, with source paths rewritten to built ones and
    # `workspace:` ranges resolved.
    tarball=$(cd "$WORKSPACE/$dir/dist" && npm pack --silent --pack-destination "$ARCHIVES")
    tarballs+=("$ARCHIVES/$tarball")
done

# ── consumer ────────────────────────────────────────────────────────────────
# Copied out of the repo so nothing above it can turn the consumer into a
# workspace member: bun and npm both walk up looking for a workspace root, and a
# single hoisted symlink would hand the consumer the source tree this is trying
# to do without.
mkdir -p "$CONSUMER"
cp "$PACKAGED_DIR/js/consumer/package.json" "$PACKAGED_DIR/js/consumer/package-lock.json" "$CONSUMER/"
cp "$PACKAGED_DIR/js/imports.mjs" "$PACKAGED_DIR/js/roundtrip.mjs" "$CONSUMER/"

echo "packaged: npm ci (frozen consumer tooling)"
(cd "$CONSUMER" && npm ci --no-audit --no-fund >/dev/null)

# The candidate-resolution step, kept explicit and separate from the frozen
# install above. Every candidate is both a direct dependency and an override, so
# a transitive `@moq/x: ^1.2.3` resolves to the staged tarball rather than to
# whatever the registry currently serves for that range.
candidate_deps=$(
    for tarball in "${tarballs[@]}"; do
        name=$(tar -xzOf "$tarball" package/package.json | jq -r .name)
        jq -n --arg k "$name" --arg v "file:$tarball" '{($k): $v}'
    done | jq -s add
)

jq --argjson deps "$candidate_deps" '
	.dependencies = ((.dependencies // {}) + $deps)
	| .overrides = ((.overrides // {}) + $deps)
' "$CONSUMER/package.json" >"$CONSUMER/package.json.next"
mv "$CONSUMER/package.json.next" "$CONSUMER/package.json"

echo "packaged: npm install (candidate archives)"
(cd "$CONSUMER" && npm install --no-audit --no-fund >/dev/null)

# An entry point behind an optional peer is still a documented entry point, and a
# consumer that imports it has the peer installed. `@moq/signals/react` is the
# case: react is optional, so npm installs nothing and the import fails on a
# package the manifest declares correctly. Install what the candidates ask for,
# at the range they ask for, rather than keeping a second list here or skipping
# the entry points those peers gate. `@moq/*` peers are left out: they are
# candidates themselves, staged as tarballs above.
peers=$(
    for name in "${candidates[@]}"; do
        jq -r '
			(.peerDependencies // {})
			| to_entries[]
			| select(.key | startswith("@moq/") | not)
			| "\(.key)@\(.value)"
		' "$CONSUMER/node_modules/$name/package.json"
    done | sort -u
)
if [[ -n "$peers" ]]; then
    echo "packaged: npm install (declared peers)"
    # shellcheck disable=SC2086  # deliberately word-split into one arg per peer
    (cd "$CONSUMER" && npm install --no-audit --no-fund $peers >/dev/null)
fi

# ── resolution ──────────────────────────────────────────────────────────────
# npm records where each installed package came from, so assert it directly
# rather than trusting that the overrides applied. A candidate served from the
# registry means the consumer tested a release that already exists, which is the
# one outcome that looks like a pass and is not one.
lock="$CONSUMER/node_modules/.package-lock.json"
[[ -f "$lock" ]] || die "npm wrote no $lock"
for name in "${candidates[@]}"; do
    registry=$(jq -r --arg name "$name" '
		.packages
		| to_entries[]
		| select(.key | endswith("node_modules/" + $name))
		| select((.value.resolved // "") | startswith("file:") | not)
		| .key
	' "$lock")
    [[ -z "$registry" ]] || die "$name resolved from the registry at: $registry"
    installed=$(jq -r --arg name "$name" '.packages["node_modules/" + $name].version // ""' "$lock")
    [[ -n "$installed" ]] || die "$name is not installed in the consumer"
done

# Symlinks are how a workspace install hides an undeclared dependency, so the
# consumer must have none. npm installs tarballs as real directories.
symlinked=$(find "$CONSUMER/node_modules/@moq" -maxdepth 1 -type l 2>/dev/null || true)
[[ -z "$symlinked" ]] || die "the consumer has symlinked @moq packages: $symlinked"

# ── entry points ────────────────────────────────────────────────────────────
# Documented entry points are the `exports` keys of the published manifest, so
# read them back out of the tarball rather than keeping a second list here.
#
# These two publish relative imports that node's ESM resolver refuses -- `tsc`
# emits specifiers as the source writes them, and these are written as directory
# and extensionless ones. The first run of this harness is what found it, and
# /quest/m0/js-published-esm-resolution.md fixes it. Listed here as an
# expectation rather than an exemption: each is still imported below, and a
# package that starts resolving fails the run until its entry here is removed.
NODE_IMPORT_BROKEN=("@moq/hang" "@moq/msf")
broken_set=$(printf '%s\n' "${NODE_IMPORT_BROKEN[@]}")

node_specs=()
browser_specs=()
for name in "${candidates[@]}"; do
    grep -qx -- "$name" <<<"$requested_set" || continue
    manifest="$CONSUMER/node_modules/$name/package.json"
    [[ "$(jq -r '.exports | type' "$manifest")" == object ]] ||
        die "$name publishes no exports map, so nothing names its entry points"
    subpaths=$(jq -r '.exports | keys[]' "$manifest")
    # A package that lists side-effectful entry points is a web-component package:
    # importing one registers a custom element and needs a DOM. Those are checked
    # by the browser bundle below instead, which is the environment they document.
    browser_only=$(jq -r 'if (.sideEffects | type) == "array" then "yes" else "no" end' "$manifest")
    for subpath in $subpaths; do
        spec="$name${subpath#.}"
        browser_specs+=("$spec")
        [[ "$browser_only" == "yes" ]] && continue
        grep -qx -- "$name" <<<"$broken_set" && continue
        node_specs+=("$spec")
    done
done

if ((${#node_specs[@]} > 0)); then
    echo "packaged: importing ${#node_specs[@]} entry points under node"
    (cd "$CONSUMER" && node imports.mjs "${node_specs[@]}")
fi

for name in "${NODE_IMPORT_BROKEN[@]}"; do
    grep -qx -- "$name" <<<"$requested_set" || continue
    broken_specs=()
    while read -r subpath; do
        broken_specs+=("$name${subpath#.}")
    done < <(jq -r '.exports | keys[]' "$CONSUMER/node_modules/$name/package.json")
    if (cd "$CONSUMER" && node imports.mjs "${broken_specs[@]}" >/dev/null 2>&1); then
        die "$name now imports under node; remove it from NODE_IMPORT_BROKEN in test/packaged/js.sh"
    fi
    echo "  xfail $name (unresolvable under node; quest/m0/js-published-esm-resolution.md)"
done

# ── browser bundle ──────────────────────────────────────────────────────────
# A browser-target bundle is the resolution a real consumer's bundler performs:
# every entry point, every transitive import, and every asset the packages
# reference (the audio worklets are inlined as blob URLs by each package's own
# vite build, and the libav WASM arrives as a normal dependency). An import the
# tarball declares but does not ship fails here.
#
# Bundled with bun rather than a bundler installed into the consumer: bun is
# already the repo's toolchain, and a tool run against the consumer resolves
# from the consumer's node_modules without becoming a dependency of it.
if ((${#browser_specs[@]} > 0)); then
    echo "packaged: bundling ${#browser_specs[@]} entry points for the browser"
    {
        echo "// Generated by test/packaged/js.sh."
        for spec in "${browser_specs[@]}"; do
            echo "export * from \"$spec\";"
        done
    } >"$CONSUMER/bundle.mjs"
    (cd "$CONSUMER" && bun build bundle.mjs --target=browser --outfile=bundle.out.mjs >/dev/null)
fi

# ── round trip ──────────────────────────────────────────────────────────────
# Resolving an entry point says the archive is complete; it does not say the
# code inside still talks to a relay. @moq/net is the layer everything else
# rides on, so the representative consumer connects to a relay built from this
# checkout, publishes a frame through it, and reads that frame back.
if grep -qx -- "@moq/net" <<<"$(printf '%s\n' "${candidates[@]}")" && [[ "${PACKAGED_ROUNDTRIP:-1}" != 0 ]]; then
    echo "packaged: building moq-relay for the round trip"
    (cd "$WORKSPACE" && "${RUST_CARGO:-cargo}" build --locked --package moq-relay)
    relay="${CARGO_TARGET_DIR:-$WORKSPACE/target}/debug/moq-relay"

    # A relay left over from an earlier run would answer the readiness poll below
    # and serve the round trip, which would pass against a build nobody staged.
    port="${PACKAGED_PORT:-4470}"
    if curl -sf "http://127.0.0.1:$port/certificate.sha256" >/dev/null 2>&1; then
        die "something is already listening on 127.0.0.1:$port (stale relay?)"
    fi

    "$relay" "$PACKAGED_DIR/js/relay.toml" \
        --server-bind "127.0.0.1:$port" --web-http-listen "127.0.0.1:$port" \
        >"$STAGE/js/relay.log" 2>&1 &
    relay_pid=$!
    trap 'kill "$relay_pid" 2>/dev/null || true; wait "$relay_pid" 2>/dev/null || true' EXIT

    for _ in $(seq 1 600); do
        curl -sf "http://127.0.0.1:$port/certificate.sha256" >/dev/null 2>&1 && break
        sleep 0.05
    done
    curl -sf "http://127.0.0.1:$port/certificate.sha256" >/dev/null 2>&1 || {
        sed 's/^/  relay: /' "$STAGE/js/relay.log" >&2 || true
        die "the relay never became ready on 127.0.0.1:$port"
    }

    # bun rather than node: @moq/web-transport ships TypeScript sources, which
    # node refuses to strip inside node_modules. The entry-point imports above
    # are the part that has to run under node, and they do.
    echo "packaged: round trip through 127.0.0.1:$port"
    if ! (cd "$CONSUMER" && bun roundtrip.mjs --url "http://127.0.0.1:$port"); then
        sed 's/^/  relay: /' "$STAGE/js/relay.log" >&2 || true
        die "the packaged @moq/net could not round trip a frame"
    fi
fi

# ── report ──────────────────────────────────────────────────────────────────
echo
echo "packaged: js archives"
for tarball in "${tarballs[@]}"; do
    name=$(tar -xzOf "$tarball" package/package.json | jq -r '"\(.name) \(.version)"')
    printf '  %-24s %s  %s\n' "$name" "$(sha256_of "$tarball")" "$(basename "$tarball")"
done
echo
echo "packaged: consumer manifest $CONSUMER/package.json"
echo "packaged: consumer command  cd $CONSUMER && npm ci && npm install && node imports.mjs ..."
