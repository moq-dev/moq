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
mod dart
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
    exec rs/scripts/bench.sh "$BASE"

# A linked worktree's Git metadata does not live under its own root: the
# per-worktree directory is `--git-dir` and everything shared (objects, remote
# refs, the branch namespace) is under `--git-common-dir`, which for an agent
# checkout is inside the main repository. Write access to the source tree
# therefore says nothing about whether this checkout can fetch, branch, or
# rebase; the answer is a property of those two directories, and finding out by
# running `git fetch` and reading the error is the slow way.
#
# `check` reports; `setup` fetches, points the branch at its base, and records
# the SHA it fetched under the per-worktree Git directory, where `check` reads it
# back to say how stale the recorded base has become. Neither ever resets,
# rebases, cleans, or checks anything out: a dirty tree is someone's work in
# progress, and adopting a checkout must not be able to destroy it.
#
# BASE follows the same rule as the rest of the repo (see `_base`): `main`
# unless the branch's upstream says otherwise.

# Report a worktree's base, Git metadata access, and state; `setup` also fetches.
worktree ACTION="check" $BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

    case "{{ ACTION }}" in
    	check | setup) ;;
    	*)
    		echo "usage: just worktree [check|setup] [BASE]" >&2
    		exit 2
    		;;
    esac

    root=$(git rev-parse --show-toplevel)
    git_dir=$(cd "$(git rev-parse --git-dir)" && pwd)
    common_dir=$(cd "$(git rev-parse --git-common-dir)" && pwd)
    branch=$(git branch --show-current || true)

    # A written probe rather than `[[ -w ]]`: the directory can be readable and
    # nominally writable while the sandbox, a read-only mount, or an ACL refuses
    # the create, and it is the create that fetch and branch creation need.
    access() {
    	local dir="$1" probe
    	[[ -d "$dir" ]] || { echo missing; return; }
    	[[ -r "$dir" ]] || { echo denied; return; }
    	probe="$dir/.moq-access-probe.$$"
    	if (umask 077 && : > "$probe") 2>/dev/null; then
    		rm -f "$probe"
    		echo write
    	else
    		echo read-only
    	fi
    }

    # A directory git will create on demand is only as writable as its nearest
    # existing parent, so probe upward instead of reporting it missing.
    access_or_parent() {
    	local dir="$1" parent
    	while [[ ! -d "$dir" ]]; do
    		parent=$(dirname "$dir")
    		[[ "$parent" != "$dir" ]] || { echo missing; return; }
    		dir="$parent"
    	done
    	access "$dir"
    }

    base=$(just _base "$BASE")
    stamp="$git_dir/moq-base"
    # Whichever remote provides the base, not always origin. A base whose first
    # segment names no remote (a local branch, a tag) leaves origin.
    remote="${base%%/*}"
    git remote | grep -qx "$remote" || remote=origin

    objects=$(access "$common_dir/objects")
    heads=$(access "$common_dir/refs/heads")
    worktree_meta=$(access "$git_dir")
    worktree=$(access "$root")
    # Tracking is `branch.<name>.remote`/`.merge` in the repository config, which
    # lives in the common directory alongside the `config.lock` the write needs,
    # not in the branch's ref. Probing refs/heads for it would refuse on a
    # writable config and, worse, proceed on a read-only one.
    config=$(access "$common_dir")
    # A fetch writes three places, not one: objects, the remote-tracking refs it
    # updates, and FETCH_HEAD in the per-worktree directory. The refs are the
    # narrow one: `refs/remotes/<remote>` is where `<branch>.lock` is created, so
    # probing `refs/remotes` stops a directory short and passes a split that git
    # then fails on.
    tracking=$(access_or_parent "$common_dir/refs/remotes/$remote")

    echo "worktree:    $root"
    echo "branch:      ${branch:-(detached)} $(git rev-parse --short HEAD)"
    echo "git-dir:     $git_dir ($worktree_meta)"
    echo "common-dir:  $common_dir ($config)"
    echo "  fetch needs $common_dir/objects: $objects"
    echo "             $common_dir/refs/remotes/$remote: $tracking"
    echo "             $git_dir (FETCH_HEAD): $worktree_meta"
    echo "  branch needs $common_dir/refs/heads: $heads"
    echo "  upstream needs $common_dir/config: $config"
    echo "  rebase needs $git_dir: $worktree_meta"
    echo "               $root: $worktree"
    echo "               $common_dir/refs/heads: $heads"

    dirty=$(git status --porcelain | wc -l | tr -d ' ')
    echo "dirty:       $dirty tracked/untracked path(s)"

    if [[ "{{ ACTION }}" == setup ]]; then
    	blocked=""
    	[[ "$objects" == write ]] || blocked="$blocked $common_dir/objects ($objects)"
    	[[ "$tracking" == write ]] || blocked="$blocked $common_dir/refs/remotes/$remote ($tracking)"
    	[[ "$worktree_meta" == write ]] || blocked="$blocked $git_dir ($worktree_meta)"
    	if [[ -n "$blocked" ]]; then
    		echo "error: cannot fetch;$blocked" >&2
    		echo "       grant write access to the main repository's Git directory, not just this worktree" >&2
    		exit 1
    	fi
    	# The remote resolved above, not always origin: recording a stamp against a
    	# ref nobody refreshed is worse than recording none, and `just check` would
    	# scope the branch against a base that has since moved.
    	git fetch --quiet "$remote"
    	# Repointing an upstream someone chose would silently change what `just
    	# check` scopes against, so only three cases write it: no upstream, an
    	# upstream `_base` discards anyway (the branch's own remote copy, which
    	# `git push -u` leaves behind and which says nothing about what the branch
    	# merges into), and a base the caller named on the command line.
    	upstream=$(git rev-parse --abbrev-ref '@{upstream}' 2> /dev/null || true)
    	# Two things stop the write, and they fail identically: a detached HEAD has
    	# no branch to hang an upstream on, and the config it lands in may be
    	# read-only. Skipping either silently would report a setup that recorded
    	# the caller's base while `just check` still scoped against origin/main.
    	blocker=""
    	if [[ -z "$branch" ]]; then
    		blocker="HEAD is detached"
    	elif [[ "$config" != write ]]; then
    		blocker="$common_dir/config is $config"
    	fi
    	if [[ -z "$upstream" ]] || [[ "$upstream" == */"$branch" ]] || [[ -n "$BASE" ]]; then
    		if [[ -z "$blocker" ]]; then
    			git branch --set-upstream-to "$base" "$branch"
    		elif [[ "$base" == origin/main ]]; then
    			# Nothing is lost: with no upstream `_base` falls back to
    			# origin/main, which is what this would have written.
    			echo "warning: cannot record the upstream; $blocker" >&2
    		else
    			# The upstream is the only place this choice survives, so a setup
    			# that could not write it did not do what it was asked.
    			echo "error: cannot set the upstream to $base; $blocker" >&2
    			echo "       the branch would keep scoping against origin/main" >&2
    			exit 1
    		fi
    	fi
    	# Resolved, written, then renamed into place. A redirect straight into the
    	# stamp truncates it before git runs, so a failure there would leave an
    	# empty file that reads back as a recorded base that never existed. Each
    	# setup needs its own temporary file so concurrent runs cannot rename or
    	# overwrite each other's in-progress stamp.
    	if [[ "$worktree_meta" == write ]]; then
    		stamp_tmp=$(mktemp "$git_dir/.moq-base.XXXXXXXX")
    		if ! git rev-parse "$base" > "$stamp_tmp"; then
    			rm -f "$stamp_tmp"
    			exit 1
    		fi
    		mv "$stamp_tmp" "$stamp"
    	fi
    fi

    if ! git rev-parse --verify --quiet "$base^{commit}" > /dev/null; then
    	echo "base:        $base (NOT FETCHED; run 'just worktree setup')"
    	exit 0
    fi

    head=$(git rev-parse "$base")
    echo "base:        $base $(git rev-parse --short "$base")"
    echo "upstream:    $(git rev-parse --abbrev-ref '@{upstream}' 2> /dev/null || echo '(unset)')"
    echo "behind:      $(git rev-list --count "HEAD..$base") commit(s)"

    recorded=""
    [[ -f "$stamp" ]] && recorded=$(cat "$stamp")

    # A stamp that no longer names a commit is worse than none: reporting it would
    # abort here on the `rev-parse --short` rather than say what to do about it.
    # An interrupted setup, or a base garbage-collected out of the repository.
    if [[ -z "$recorded" ]]; then
    	echo "recorded:    (none; run 'just worktree setup')"
    elif ! git rev-parse --verify --quiet "$recorded^{commit}" > /dev/null; then
    	echo "recorded:    $recorded (UNKNOWN COMMIT; run 'just worktree setup')"
    elif [[ "$recorded" == "$head" ]]; then
    	echo "recorded:    $(git rev-parse --short "$recorded") (current)"
    else
    	echo "recorded:    $(git rev-parse --short "$recorded") (STALE; $base has moved since setup)"
    fi

# Install repo-wide tooling. Per-language deps install on first check.
install:
    bun install
    cargo install --locked cargo-shear cargo-sort cargo-upgrades cargo-edit cargo-semver-checks release-plz

# Resolve BASE: arg > $GITHUB_BASE_REF > upstream > origin/main. A branch's
# upstream is the branch it merges into, which is the base a `dev`-targeted
# branch needs. `git push -u` repoints upstream at the branch's own remote copy,
# which would diff HEAD against itself, so ignore that case (see CLAUDE.md).
# GITHUB_BASE_REF outranks the upstream because a PR checkout has no upstream
# configured, and the branch being merged into is exactly the base GitHub is
# asking about.
#
# Shared by `_changed` and `worktree`, so a checkout's scope and its reported
# base can never disagree.

# Print the ref this branch is based on.
[private]
_base $BASE="":
    #!/usr/bin/env bash
    set -euo pipefail

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
    printf '%s\n' "$base"

# Reports the base it picked on stderr, so a surprising scope is traceable.
#
# LIMIT is the byte budget for the printed list, and exists as a parameter so
# `_changed-test` can force the oversized path without a synthetic 30k-file diff.

# Print the files this branch changed relative to BASE, one per line, or `ALL`.
[private]
_changed $BASE $LIMIT=changed_max:
    #!/usr/bin/env bash
    set -euo pipefail

    base=$(just _base "$BASE")

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
# Locally it warns rather than failing, because the alternative is the thing
# this exists to prevent: a green `just check` that skipped half its linters and
# reads exactly like one that ran them.
#
# The scope-to-tool mapping lives in `scripts/doctor.sh --tools`, which `just
# doctor` reads as well. Two copies would drift, and the one that drifted
# quietly would be the one turning a skip into a pass.

# Fail (under MOQ_STRICT) or warn when a tool the diff's scopes need is missing.
[private]
_tools $FILES="" $SUITE="check":
    #!/usr/bin/env bash
    set -euo pipefail

    case "$SUITE" in
        check) mode=--tools ;;
        test) mode=--test-tools ;;
        *) echo "error: unknown tool suite: $SUITE" >&2; exit 2 ;;
    esac

    if ! tools=$(scripts/doctor.sh "$mode" "$FILES"); then
        echo "error: scripts/doctor.sh $mode failed; the required tool set is unknown" >&2
        exit 1
    fi
    if [[ -z "$tools" ]]; then
        if [[ "$SUITE" == check ]]; then
            echo "error: scripts/doctor.sh $mode returned no tools; the mapping is broken" >&2
            exit 1
        fi
        exit 0
    fi

    missing=()
    while IFS= read -r tool; do
        [[ -n "$tool" ]] || continue
        command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
    done <<< "$tools"

    ((${#missing[@]})) || exit 0

    if [[ -n "${MOQ_STRICT:-}" ]]; then
        echo "error: MOQ_STRICT is set but these tools are missing: ${missing[*]}" >&2
        echo "       run inside 'nix develop', or unset MOQ_STRICT to skip what isn't installed" >&2
        exit 1
    fi

    echo "warning: missing tools, so whatever needs them is SKIPPED, not checked: ${missing[*]}" >&2
    echo "         this run is not complete verification; 'just doctor' says what each one blocks." >&2

# Diagnosis only: it never installs a tool, approves an .envrc, or widens a
# sandbox, and every probe carries a budget, so a hung daemon or an unreachable
# network costs seconds rather than the run. `--json` for a machine reader.

# Report which verification suites this checkout can run, and why one cannot.
doctor *args:
    scripts/doctor.sh {{ args }}

# The parts whose bugs are invisible in a healthy environment, which is every
# environment that would otherwise notice: the budget, the permission
# classifier, the JSON encoder, and the tool mapping `_tools` now depends on.

# Check the doctor's own classifier, budget, encoder, and tool mapping.
[private]
_doctor-test:
    scripts/doctor.sh --self-test

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
        # Quest documents form one graph, so validate the whole living tree when
        # either a quest or its validator changes.
        if echo "$files" | grep -qE '^(quest/|rs/quest/)'; then
            cargo run --quiet --locked --package quest -- check
        fi
        just py check "$files"
        just kt check "$files"
        just swift check "$files"
        just go check "$files"
        just dart check "$files"
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
        # flake.lock too, because the third OBS the guard compares is nixpkgs'
        # obs-studio, the one `just obs ci` links: it moves on a lock bump alone,
        # and that bump is the change that opens the gap.
        if echo "$files" | grep -qE '^(cpp/obs/|flake\.(nix|lock)$)'; then
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
    cargo run --quiet --locked --package quest -- check
    # Not covered by the line above: moq-wasm only exists on the wasm32 target.
    just rs wasm
    just py check
    just kt check
    just swift check
    just go check
    just dart check
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

# remark-cli has no `--check`. `--frail` raises the exit code on lint messages,
# and only `--output` formats, so a file that is merely misformatted passes both
# ways. Format a scratch mirror and diff that, so the check stays read-only
# while `fix` still writes in place.

# Run Markdown lints over the worktree, either checking formatting or applying it.
[private]
_markdown $ACTION:
    #!/usr/bin/env bash
    set -euo pipefail

    case "$ACTION" in
        check) ;;
        fix)
            bun remark . --quiet --output
            exit 0
            ;;
        *)
            echo "invalid markdown action: $ACTION" >&2
            exit 2
            ;;
    esac

    mirror=$(mktemp -d)
    trap 'rm -rf "$mirror"' EXIT

    extension_list=$(bun -e \
        'import extensions from "markdown-extensions"; console.log(extensions.join("\n"))')
    patterns=()
    while IFS= read -r extension; do
        [[ -n "$extension" ]] && patterns+=("*.$extension")
    done <<< "$extension_list"
    ((${#patterns[@]})) || {
        echo "error: remark reported no Markdown extensions" >&2
        exit 1
    }

    # Untracked files are in scope so a new doc is linted before it is staged;
    # --exclude-standard keeps build output out.
    files=()
    while IFS= read -r -d '' file; do
        [[ -f "$file" ]] || continue
        files+=("$file")
        mkdir -p "$mirror/$(dirname "$file")"
        cp "$file" "$mirror/$file"
    done < <(git ls-files -z --cached --others --exclude-standard -- "${patterns[@]}")
    ((${#files[@]})) || exit 0

    # The config rides along so every .remarkignore pattern resolves against the
    # mirror root exactly as it does here, and node_modules is where the plugins
    # named by .remarkrc.mjs come from.
    cp .remarkrc.mjs .remarkignore "$mirror/"
    ln -s "$PWD/node_modules" "$mirror/node_modules"

    status=0
    (cd "$mirror" && bun remark . --quiet --frail --output) || status=$?

    stale=()
    for file in "${files[@]}"; do
        cmp -s "$file" "$mirror/$file" || stale+=("$file")
    done

    if ((${#stale[@]})); then
        echo "error: these files are not formatted, run 'just fix':" >&2
        printf '       %s\n' "${stale[@]}" >&2
        status=1
    fi

    exit "$status"

# Check Markdown formatting detection without touching the caller's worktree.
[private]
_markdown-test:
    #!/usr/bin/env bash
    set -euo pipefail

    fail() { echo "markdown: _markdown-test: $1" >&2; exit 1; }

    repo=$PWD
    fixture=$(mktemp -d)
    trap 'rm -rf "$fixture"' EXIT

    git -C "$fixture" init --quiet
    cp .remarkrc.mjs .remarkignore "$fixture/"
    ln -s "$repo/node_modules" "$fixture/node_modules"
    for file in dirty.md dirty.markdown; do
        printf '# dirty_heading\n' > "$fixture/$file"
        git -C "$fixture" add "$file"
    done

    original='# dirty_heading'
    if just --justfile "$repo/justfile" --working-directory "$fixture" \
        _markdown check > "$fixture/check.log" 2>&1; then
        fail "formatter-only drift must fail the check"
    fi
    for file in dirty.md dirty.markdown; do
        [[ "$(<"$fixture/$file")" == "$original" ]] \
            || fail "the check modified $file"
        grep -q "$file" "$fixture/check.log" \
            || fail "the check did not name $file"
    done

    just --justfile "$repo/justfile" --working-directory "$fixture" _markdown fix
    for file in dirty.md dirty.markdown; do
        [[ "$(<"$fixture/$file")" == '# dirty\_heading' ]] \
            || fail "the fix did not format $file"
    done
    just --justfile "$repo/justfile" --working-directory "$fixture" _markdown check

    echo "markdown: check/fix regression ok"

# Repository-wide lints, shared by `check` and `check-all`.
[private]
_check-common:
    just _changed-test
    just _doctor-test
    bun install --frozen-lockfile
    just _markdown-test
    just _markdown check
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
    	just dart fix "$files"
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
    just dart fix
    just obs fix
    just _fix-common

# Optional tools skip if missing locally. `bun install` for the same reason as
# `_check-common`.

# Repository-wide fixes, shared by `fix` and `fix-all`.
[private]
_fix-common:
    bun install
    just _markdown fix
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

# Only this checkout by default. Agent worktrees each carry their own artifacts
# now that the shared target dir is gone, and another agent is usually building
# in one right now: `cargo clean` under a running build fails it, and there is no
# way to tell a finished worktree from a busy one from out here. `just clean all`
# is the explicit opt-in for a machine the caller knows is idle.
#
# Source is never touched either way, dirty or untracked: this deletes build
# output, not work. Nothing here reaches a machine-wide store -- no Nix garbage
# collection, no cargo/bun/uv home cache -- because those are shared with every
# other checkout and rebuilding them costs far more than the space they hold.

# Delete this checkout's build artifacts and caches; `all` includes agent worktrees.
clean SCOPE="here":
    #!/usr/bin/env bash
    set -euo pipefail

    case "{{ SCOPE }}" in
    	here | all) ;;
    	*)
    		echo "usage: just clean [here|all]" >&2
    		exit 2
    		;;
    esac

    just rs clean
    just js clean
    just py clean
    just kt clean
    just swift clean
    just go clean
    just dart clean

    # Caches not owned by any one language: nix build result, direnv, wrangler.
    rm -rf result .direnv
    find . -name .claude -prune -o -type d -name .wrangler -prune -exec rm -rf {} +

    # Worktrees don't nest, so this recurses exactly one level. Tolerate stale
    # worktrees on branches that predate this recipe.
    if [[ "{{ SCOPE }}" == all ]]; then
    	for wt in .claude/worktrees/*/; do
    		[ -f "${wt}justfile" ] || continue
    		echo "==> cleaning ${wt}"
    		(cd "$wt" && just clean) || echo "    (skipped: just clean failed in ${wt})"
    	done
    fi

# Upgrade any tooling
update:
    just js update
    just rs update
    nix flake update

# Serve the documentation locally.
doc:
    cd doc && bun run dev
