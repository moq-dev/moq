#!/usr/bin/env just --justfile
# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation

set unstable

# Plain `cargo` unless set. See rs/justfile for the local wrapper option.
_rust_cargo := env_var_or_default("RUST_CARGO", "cargo")
cargo_compile := if _rust_cargo == "" { "cargo" } else { _rust_cargo }

# Per-language modules. Language-specific recipes live in their own justfiles.
mod js
mod rs
mod py
mod kt
mod swift
mod go
# OBS Studio plugin (C++). See doc/bin/obs.md.
mod obs 'cpp/obs'
# Unit tests per language (`just test`).
mod test
# Demos and infra.
mod demo
mod infra
# IETF Internet-Drafts (`just drafts build`, `just drafts publish`).
mod drafts
# GitHub Actions workflow linting.
mod gh '.github'
# Shortcuts to avoid `demo::` prefix.
mod boy 'demo/boy'
mod pub 'demo/pub'
mod relay 'demo/relay'
mod sub 'demo/sub'
mod web 'demo/web'

# Byte budget for a changed-file list, sized so it survives being passed as a
# single argv/env string on every hop of the dispatch. See `_changed`.
changed_max := '65536'

# Run the demo by default.
default:
    just demo

# Alias for `just demo`.
dev:
    just demo

# Benchmark the current tree, or compare it with a commit: `just bench origin/main`.
bench $BASE="":
    #!/usr/bin/env bash
    exec just --justfile bench/justfile compare "$BASE"

# Compare one multi-threaded Tokio runtime with the same number of independent
# Tokio/epoll and io_uring workers. Defaults to every logical CPU.
bench-runtime $ROUNDS="3" $WORKERS="":
    #!/usr/bin/env bash
    exec just --justfile bench/justfile runtime "$ROUNDS" "$WORKERS"

# Install repo-wide tooling. Per-language deps install on first check.
install:
    bun install
    cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit cargo-semver-checks release-plz

# Reports the base it picked on stderr, so a surprising scope is traceable.
#
# LIMIT is the byte budget for the printed list, and exists as a parameter so
# `_changed-test` can force the oversized path without a synthetic 30k-file diff.

# Print the files this branch changed relative to BASE, one per line, or `ALL`.
[private]
_changed $BASE $LIMIT=changed_max:
    #!/usr/bin/env bash
    set -euo pipefail

    # Resolve BASE: arg > $GITHUB_BASE_REF > upstream > origin/main. A branch's
    # upstream is the branch it merges into, which is the base a `dev`-targeted
    # branch needs. `git push -u` repoints upstream at the branch's own remote
    # copy, which would diff HEAD against itself, so ignore that case (see
    # CLAUDE.md). GITHUB_BASE_REF outranks the upstream because a PR checkout
    # has no upstream configured, and the branch being merged into is exactly
    # the base GitHub is asking about.
    base="$BASE"
    if [[ -z "$base" && -n "${GITHUB_BASE_REF:-}" ]]; then
    	base="origin/${GITHUB_BASE_REF}"
    fi
    if [[ -z "$base" ]]; then
    	base=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null || true)
    	if [[ -z "$base" || "$base" == */"$(git branch --show-current)" ]]; then
    		base="origin/main"
    	fi
    fi

    merge_base=$(git merge-base "$base" HEAD) || {
    	echo "error: cannot resolve merge-base against $base (is full history fetched?)" >&2
    	exit 1
    }
    echo "base: $base" >&2

    # Untracked files count too: a brand new crate or module is the whole change.
    files=$({
    	git diff --name-only "$merge_base"
    	git ls-files --others --exclude-standard
    } | sort -u)

    # Assigned rather than tested inline, so a failing `_changed-cap` aborts here
    # under `set -e`. Inside `[[ ]]` its exit status is discarded, and the caller
    # would then scope to a list that never got budgeted -- straight back into the
    # E2BIG this exists to prevent.
    cap=$(printf '%s' "$files" | just _changed-cap "$LIMIT")
    if [[ "$cap" == ALL ]]; then
    	echo ALL
    	exit 0
    fi

    # Guarded because a bare printf of an empty list prints a newline, and the
    # callers test the result for emptiness to decide whether anything changed.
    if [[ -n "$files" ]]; then
    	printf '%s\n' "$files"
    fi

# Every hop of the dispatch takes the changed-file list as one argument, and
# just exports recipe parameters into the child's environment, so the whole list
# has to fit in a single execve string. Linux caps one string at MAX_ARG_STRLEN
# (32 pages, 131072 bytes) however large ARG_MAX is, so past that each hop dies
# with E2BIG, which just reports as exit code 126 and no mention of the diff. A
# diff that large selects most of the workspace anyway, so the callers widen to
# the unscoped suite, which passes no list at all.
#
# Split out from `_changed` so `_changed-test` can drive the decision with a
# synthetic list, rather than needing the working tree to hold a diff of a
# particular size.
#
# The list arrives on stdin, which is both the only channel that can carry an
# oversized one and the only way to measure it honestly: `${#var}` counts
# CHARACTERS under a UTF-8 locale while execve counts BYTES, so a path set of
# 3-byte characters would read as a third of its real size and sail past a
# budget it actually blows.

# Print `ALL` when the changed-file list on stdin is too long for one argument.
[private]
_changed-cap $LIMIT=changed_max:
    #!/usr/bin/env bash
    set -euo pipefail

    [[ "$LIMIT" =~ ^[0-9]+$ ]] || {
    	echo "changed: not a byte count: $LIMIT" >&2
    	exit 2
    }

    bytes=$(wc -c | tr -d '[:space:]')
    if ((bytes > LIMIT)); then
    	echo "changed: $bytes bytes of paths exceeds the $LIMIT budget; selecting everything." >&2
    	echo ALL
    fi

# Guards the thing that fails LOUDLY but unhelpfully: past the budget every
# `just` hop dies with "Argument list too long" and exit 126, naming neither the
# diff nor the recipe that could not receive it. Both halves matter -- a budget
# that never trips scopes nothing, and one above what execve accepts still dies.

# Check that an oversized diff widens to `ALL`, and that the budget fits in argv.
[private]
_changed-test $LIMIT=changed_max:
    #!/usr/bin/env bash
    set -euo pipefail

    fail() { echo "changed: _changed-test: $1" >&2; exit 1; }

    # Synthetic sizes rather than whatever the working tree happens to hold: a
    # clean checkout has no diff at all, and `check-all` runs there (cache.yml
    # warms the cache from `main`), so a test keyed on the real list would take
    # down the one job allowed to write the shared Rust cache.
    [[ "$(printf 'aaaa' | just _changed-cap 3)" == ALL ]] || fail "over budget must print ALL"
    [[ -z "$(printf 'aaa' | just _changed-cap 3)" ]] || fail "at budget must print nothing"
    [[ -z "$(printf '' | just _changed-cap 3)" ]] || fail "an empty diff must print nothing"

    # The smallest nonempty list there is, against the only budget below it.
    [[ "$(printf 'x' | just _changed-cap 0)" == ALL ]] || fail "a 1-byte list must exceed a 0 budget"

    # execve counts bytes, so the budget has to as well. Spelled as raw bytes
    # rather than as characters: this is one CJK character, 3 bytes wide, which
    # `${#var}` would count as 1 under a UTF-8 locale and let past a 2-byte
    # budget it actually blows.
    [[ "$(printf '\xe6\x97\xa5' | just _changed-cap 2)" == ALL ]] \
    	|| fail "the budget must count bytes, not characters"

    # A byte count is the whole input, so anything else is a caller bug, not a
    # reason to silently scope to nothing.
    ! printf '' | just _changed-cap not-a-number 2> /dev/null || fail "a bad budget must be rejected"

    # ...and that rejection has to reach the caller. A swallowed one would hand
    # back an unbudgeted list, which is the failure this whole recipe prevents.
    ! just _changed "" not-a-number 2> /dev/null || fail "a rejected budget must fail _changed"

    # Linux caps a single argv/env string at MAX_ARG_STRLEN (32 pages), whatever
    # ARG_MAX says, and the list travels as one string. Asserted rather than
    # probed because this repo's CI is the Linux host and a dev box may be laxer.
    ((LIMIT <= 131072)) || fail "budget $LIMIT exceeds Linux MAX_ARG_STRLEN"

    # The budget still has to survive the hop it was sized for, which the checks
    # above cannot show: they never pass a list that big to anything.
    payload=$(head -c "$LIMIT" /dev/zero | tr '\0' x)
    [[ "$(just _echo "$payload" | wc -c)" -eq $((LIMIT + 1)) ]] \
    	|| fail "a $LIMIT-byte list does not survive an argv hop"

    # End to end, but only when there is a diff to be oversized: see above. The
    # budget is zero rather than one because the real list is whatever the
    # checkout holds, and a single one-character root path is a one-byte list
    # that a one-byte budget does not exceed. Zero is below every nonempty list.
    if [[ -n "$(just _changed "" 100000000)" ]]; then
    	[[ "$(just _changed "" 0)" == ALL ]] || fail "a diff over budget must print ALL"
    fi

    echo "changed: budget ok"

# Print an argument back, to measure what survives a `just` invocation.
[private]
_echo $VALUE:
    @printf '%s\n' "$VALUE"

# Tools every scope guards with `command -v`, so an incomplete local toolchain
# checks less instead of failing. That trade is wrong in CI, where a skip is
# indistinguishable from a pass, so CI exports MOQ_STRICT=1 and this turns the
# required set into a precondition. Checked up front, and as one list, so a
# missing tool is reported before a long compile rather than after it.
#
# Required per scope, mirroring what `check` actually dispatches for a given
# diff: demanding gradle on a docs-only PR would fail a run that was never going
# to invoke it. Takes the same file list as the dispatch, or `ALL` to require
# everything (`check-all`).
#
# One deliberate absence: swift exists only on macOS, and `swift check` skips
# off-macOS by design; swift.yml is its real gate.

# Fail when a tool the diff's scopes need is missing. No-op unless MOQ_STRICT.
[private]
_tools $FILES="":
    #!/usr/bin/env bash
    set -euo pipefail
    [[ -n "${MOQ_STRICT:-}" ]] || exit 0

    scoped() { [[ "$FILES" == ALL ]] || grep -qE "$1" <<< "$FILES"; }

    # `_check-common` runs on every invocation, so its tools are unconditional.
    tools=(actionlint bun jq nix nixfmt shellcheck shfmt taplo)
    scoped '^(bench/|quest/|rs/|Cargo\.(toml|lock)$|rust-toolchain\.toml$)' && tools+=(cargo)
    scoped '^(py/|pyproject\.toml$|uv\.lock$|rs/moq-ffi/)'     && tools+=(uv)
    scoped '^(kt/|rs/moq-ffi/)'                                && tools+=(gradle java)
    # cargo because `go check` builds moq-ffi for the host, and skips on a
    # missing cargo the same way it skips on a missing go. rsync because the
    # publish scripts stage the mirror tree with it, so the publisher test skips
    # without it, and a skip that keeps `just check` green is what MOQ_STRICT is
    # here to prevent.
    scoped '^(go/|rs/moq-ffi/)'                                && tools+=(go uniffi-bindgen-go cargo rsync)
    # cargo regenerates moq.h for the type-check; pkg-config locates Qt6 and
    # ffmpeg. Every platform: the plugin type-checks against headers, and the
    # dev shell ships those even on Darwin, where obs-studio can't build.
    scoped '^(cpp/obs/|rs/libmoq/)' && tools+=(clang-format gersemi pkg-config cargo)

    # Scopes overlap (rs/moq-ffi/ is in four of them), so the same tool can land
    # in the list twice and be reported missing twice. Splitting on whitespace is
    # safe: every entry is a bare command name.
    tools=($(printf '%s\n' "${tools[@]}" | sort -u))

    missing=()
    for tool in "${tools[@]}"; do
    	command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
    done
    if ((${#missing[@]})); then
    	echo "error: MOQ_STRICT is set but these tools are missing: ${missing[*]}" >&2
    	echo "       run inside 'nix develop', or unset MOQ_STRICT to skip what isn't installed" >&2
    	exit 1
    fi

# Lints and compiles only the packages the branch changed plus everything
# depending on them, so several worktrees can build at once. This is also what
# CI runs (with MOQ_STRICT=1), so there is no second, drifting definition of
# "checked". Tests are the sibling `just test`; `check-all` is the unscoped suite.

# Lint and compile what the branch changed since BASE, plus its dependents.
check $BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

    files=$(just _changed "$BASE")

    # `_changed` says ALL when the list outgrew what argv can carry. The unscoped
    # suite is the path that passes no list at all, so it is the one that works.
    if [[ "$files" == ALL ]]; then
    	just check-all
    	exit 0
    fi

    just _tools "$files"

    # The dispatch below lives in these two files, and neither matches any
    # language scope, so a PR that rewrites how CI dispatches would otherwise
    # validate none of it. Hand off to the unscoped suite instead.
    if grep -qE '^(justfile|test/justfile)$' <<< "$files"; then
    	echo "check: root orchestration changed; checking everything." >&2
    	just check-all
    	exit 0
    fi

    # An empty list means "force-run" to the per-lang recipes, which is the
    # wrong semantic here, so don't dispatch at all.
    if [[ -n "$files" ]]; then
        just js check "$files"
        just rs check-changed "$files"
        if echo "$files" | grep -q '^bench/'; then
            just --justfile bench/justfile check
        fi
        # Quest documents form one graph, so validate the whole living tree when
        # either a quest or its validator changes.
        if echo "$files" | grep -qE '^(quest/|rs/quest/)'; then
            cargo run --quiet --locked --package quest -- check
        fi
        just py check "$files"
        just kt check "$files"
        just swift check "$files"
        just go check "$files"
    	# Type-checking the plugin needs only headers, so it runs here rather
    	# than waiting for obs.yml to link it on Linux. libmoq is in scope
    	# because the plugin calls through its generated C header, and flake.nix
    	# because it owns the libobs headers this compiles against -- obs.yml
    	# links against nixpkgs' obs-studio instead, so nothing else would notice
    	# that package going bad.
    	if echo "$files" | grep -qE '^(cpp/obs/|rs/libmoq/|flake\.nix$)'; then
    		just obs compile
    	fi
    	# flake.nix is in scope because `just obs check` is what compares the OBS
    	# version pinned there against buildspec.json, and either side can move.
    	if echo "$files" | grep -qE '^(cpp/obs/|flake\.nix$)'; then
    		just obs check
    	fi
    	# Validates flake eval + dev shell build; it no longer compiles the
    	# workspace, so it's cheap. Gated anyway: a pure doc/JS PR can't
    	# affect flake eval.
    	if echo "$files" | grep -qE '(^rs/|^Cargo\.(toml|lock)$|^flake\.lock$|\.nix$)'; then
    		just _flake
    	fi
    else
    	echo "check: nothing changed."
    fi

    just _check-common

# Check every package in every language, plus moq-wasm.
check-all *args:
    just _tools ALL
    just js check
    just rs check --workspace {{ args }}
    just rs tokio-features
    just --justfile bench/justfile check
    cargo run --quiet --locked --package quest -- check
    # Not covered by the line above: moq-wasm only exists on the wasm32 target.
    just rs wasm
    just py check
    just kt check
    just swift check
    just go check
    just obs check
    just obs compile
    just _flake
    just _check-common

# Skips when nix is absent: the flake is not a precondition for working on the
# repo, and `_tools` already makes it required under MOQ_STRICT.

# Validate flake evaluation and the dev shell build.
[private]
_flake:
    @if command -v nix >/dev/null 2>&1; then nix flake check; fi

# Repository-wide non-compiling checks shared by `check` and `check-all`.
# Optional shell, workflow, TOML, Nix, and justfile lints skip if missing.
#
# `bun install` because remark-cli lives in node_modules and `just js check` is
# where it would otherwise be installed, which a Rust-only diff skips.

# Run shell checks or formatting over tracked files that exist in the worktree.
[private]
_shell $ACTION:
    #!/usr/bin/env bash
    set -euo pipefail

    if ! command -v shfmt >/dev/null 2>&1; then
        exit 0
    fi
    if [[ "$ACTION" == "check" ]] && ! command -v shellcheck >/dev/null 2>&1; then
        exit 0
    fi

    scripts_file=$(mktemp)
    trap 'rm -f "$scripts_file"' EXIT
    shfmt -f=0 . > "$scripts_file"

    scripts=()
    while IFS= read -r -d '' file; do
        if git --literal-pathspecs ls-files --error-unmatch -- "$file" >/dev/null 2>&1; then
            scripts+=("$file")
        fi
    done < "$scripts_file"
    ((${#scripts[@]})) || exit 0

    case "$ACTION" in
        check)
            shfmt --diff "${scripts[@]}"
            shellcheck "${scripts[@]}"
            ;;
        fix)
            shfmt --write "${scripts[@]}"
            ;;
        *)
            echo "invalid shell action: $ACTION" >&2
            exit 2
            ;;
    esac

# Repository-wide lints, shared by `check` and `check-all`.
[private]
_check-common:
    just _changed-test
    bun install --frozen-lockfile
    bun remark . --quiet --frail
    just _shell check
    @if command -v taplo >/dev/null 2>&1; then RUST_LOG=error taplo format --check; fi
    @if command -v nixfmt >/dev/null 2>&1; then nixfmt --check $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); fi
    @for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); do just --fmt --check --justfile "$f"; done
    just gh check

# Scoped exactly like `check`, because `clippy --fix` compiles what it fixes.
# `fix-all` is the unscoped version.

# Auto-fix lint and formatting for what the branch changed since BASE.
fix $BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

    files=$(just _changed "$BASE")

    # Mirrors `check`: too long for argv means fix everything instead.
    if [[ "$files" == ALL ]]; then
    	just fix-all
    	exit 0
    fi

    if [[ -n "$files" ]]; then
    	just js fix "$files"
    	just rs fix-changed "$files"
    	just py fix "$files"
    	if echo "$files" | grep -q '^cpp/obs/'; then
    		just obs fix
    	fi
    else
    	echo "fix: nothing changed."
    fi

    just _fix-common

# Auto-fix every JavaScript workspace and every default Rust member.
fix-all:
    just js fix
    just rs fix --workspace
    just py fix
    just obs fix
    just _fix-common

# Optional tools skip if missing locally. `bun install` for the same reason as
# `_check-common`.

# Repository-wide fixes, shared by `fix` and `fix-all`.
[private]
_fix-common:
    bun install
    bun remark . --quiet --output
    just _shell fix
    @if command -v taplo >/dev/null 2>&1; then RUST_LOG=error taplo format; fi
    @if command -v nixfmt >/dev/null 2>&1; then nixfmt $(find . -name '*.nix' -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); fi
    @for f in $(find . -name justfile -not -path './node_modules/*' -not -path './target/*' -not -path './.venv/*' -not -path './.direnv/*'); do just --fmt --justfile "$f"; done

# Build the packages.
build:
    just js build
    just rs build
    if command -v uv &> /dev/null; then just py build; fi
    if command -v wasm-bindgen &> /dev/null; then just wasm; fi

# Build browser/WASM bindings into @moq/wasm using the pinned wasm-bindgen toolchain.
wasm:
    {{ cargo_compile }} build --locked -p moq-wasm --target wasm32-unknown-unknown --profile wasm-release
    wasm-bindgen --target web --out-name moq \
    	--out-dir js/wasm/dist "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/wasm-release/moq_wasm.wasm"

# Delete build artifacts and caches, including per-language outputs and agent worktrees.
clean:
    #!/usr/bin/env bash
    set -euo pipefail

    just rs clean
    just js clean
    just py clean
    just kt clean
    just swift clean
    just go clean

    # Caches not owned by any one language: nix build result, direnv, wrangler.
    rm -rf result .direnv
    find . -name .claude -prune -o -type d -name .wrangler -prune -exec rm -rf {} +

    # Agent worktrees each carry their own artifacts now that the shared
    # target dir is gone. Worktrees don't nest, so this recurses exactly one
    # level. Tolerate stale worktrees on branches that predate this recipe.
    for wt in .claude/worktrees/*/; do
    	[ -f "${wt}justfile" ] || continue
    	echo "==> cleaning ${wt}"
    	(cd "$wt" && just clean) || echo "    (skipped: just clean failed in ${wt})"
    done

# Upgrade any tooling
update:
    just js update
    just rs update
    nix flake update

# Serve the documentation locally.
doc:
    cd doc && bun run dev
