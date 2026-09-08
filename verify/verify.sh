#!/usr/bin/env bash
#
# Verification receipts for the current merge candidate.
#
# `just check` and `just test` already define what passing means; this records
# what a run of one of them actually covered -- source, base, toolchain, and any
# externally supplied binary -- so a later reader can tell whether that pass
# still describes the tree and the pull request in front of them. It never
# decides what to run, and it never merges anything.
#
# Evidence kinds. A receipt carries exactly one, and a report never merges two:
#   static    lint and compile only; no behavior was executed
#   local     tests built from this checkout
#   binary    an externally supplied binary was under test
#   ci        a hosted run, read by `pr`
#   hardware  a device this checkout cannot drive, recorded by hand
#
# Environment:
#   RELAY_BIN, MOQ_BIN  Binary overrides. Digested and recorded; one that does
#                       not come from this checkout makes the run exploratory.

set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage:
  verify.sh record KIND LANE BASE -- COMMAND...  Run COMMAND, write a receipt.
  verify.sh status [--json]                      Re-grade the stored receipts.
  verify.sh pr [NUMBER] [--json]                 Read-only pull request state.
  verify.sh report [NUMBER] [--json]             Receipts joined to the PR.
  verify.sh classify                             Grade gathered JSON on stdin.
EOF
    exit 2
}

die() {
    echo "verify: $*" >&2
    exit 2
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

need git
need jq

ROOT=$(git rev-parse --show-toplevel) || die "not a git repository"
cd "$ROOT"

# Receipts live outside version control: `just _changed` counts untracked files,
# so a tracked receipt would change the very scope it describes and every run
# would invalidate the one before it.
VERIFY_DIR="$ROOT/.verify"

# Every hash here is a git blob id, because git is the one hasher this script
# can assume: sha256sum is absent on macOS, shasum on some minimal Linux images.
blob() {
    git hash-object -- "$1" 2>/dev/null || echo unreadable
}

# The identity of everything a run could have read: HEAD, the staged and
# unstaged diff against it, and the content of every untracked file. Contents
# are hashed, never stored, so an untracked secret cannot land in a receipt.
source_digest() {
    {
        git rev-parse HEAD
        git diff --binary HEAD
        git ls-files --others --exclude-standard -z | while IFS= read -r -d '' file; do
            printf '%s %s\n' "$(blob "$file")" "$file"
        done
    } | git hash-object --stdin
}

timestamp() {
    date -u +%Y-%m-%dT%H:%M:%SZ
}

# First line of `<tool> --version`, or `absent`, so a receipt names the compiler
# that produced it rather than only asserting that one existed.
tool_version() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo absent
        return
    fi
    "$@" 2>/dev/null | head -n 1 || echo unknown
}

# Whether an overridden binary can be traced to this checkout. `checkout` still
# does not prove it was built from the current source, only that the harness
# could have built it there, so any override is `binary` evidence.
binary_provenance() {
    local path=$1
    if [[ ! -e $path ]]; then
        echo missing
        return
    fi

    local target dir resolved
    target="${CARGO_TARGET_DIR:-$ROOT/target}"
    if ! dir=$(cd "$(dirname -- "$path")" && pwd -P); then
        echo external
        return
    fi
    resolved="$dir/$(basename -- "$path")"

    if [[ -d $target ]] && target=$(cd "$target" && pwd -P) && [[ $resolved == "$target"/* ]]; then
        echo checkout
        return
    fi
    echo external
}

binary_records() {
    local records='[]' variable path
    for variable in RELAY_BIN MOQ_BIN; do
        path="${!variable:-}"
        [[ -n $path ]] || continue
        records=$(jq \
            --arg variable "$variable" \
            --arg path "$path" \
            --arg digest "$(blob "$path")" \
            --arg provenance "$(binary_provenance "$path")" \
            '. + [{variable: $variable, path: $path, digest: $digest, provenance: $provenance}]' \
            <<<"$records")
    done
    printf '%s' "$records"
}

environment_record() {
    jq -n \
        --arg host "$(uname -sm)" \
        --arg cargo "$(tool_version "${RUST_CARGO:-cargo}" --version)" \
        --arg rustc "$(tool_version rustc --version)" \
        --arg target "$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' || true)" \
        --arg bun "$(tool_version bun --version)" \
        --arg just "$(tool_version just --version)" \
        --arg nix_shell "${IN_NIX_SHELL:-}" \
        --arg rust_cargo "${RUST_CARGO:-}" \
        --arg cargo_target_dir "${CARGO_TARGET_DIR:-}" \
        --arg strict "${MOQ_STRICT:-}" \
        --arg nextest_profile "${NEXTEST_PROFILE:-}" \
        --arg cargo_lock "$(blob Cargo.lock)" \
        --arg bun_lock "$(blob bun.lock)" \
        --arg flake_lock "$(blob flake.lock)" \
        --arg toolchain "$(blob rust-toolchain.toml)" \
        '{
            host: $host,
            cargo: $cargo,
            rustc: $rustc,
            target: $target,
            bun: $bun,
            just: $just,
            locks: {
                "Cargo.lock": $cargo_lock,
                "bun.lock": $bun_lock,
                "flake.lock": $flake_lock,
                "rust-toolchain.toml": $toolchain
            },
            variables: {
                IN_NIX_SHELL: $nix_shell,
                RUST_CARGO: $rust_cargo,
                CARGO_TARGET_DIR: $cargo_target_dir,
                MOQ_STRICT: $strict,
                NEXTEST_PROFILE: $nextest_profile
            }
        }'
}

cmd_record() {
    (($# >= 4)) || usage
    local kind=$1 lane=$2 base=$3
    shift 3
    # The recipes pass their own `--`, and a caller who types one gets a second.
    # `--` is never a command, so eat as many as arrive.
    while [[ ${1:-} == -- ]]; do
        shift
    done
    (($#)) || die "record needs a command"

    case $kind in
        static | local | binary | ci | hardware) ;;
        *) die "unknown kind: $kind (static, local, binary, ci, hardware)" ;;
    esac
    [[ $lane =~ ^[a-z0-9][a-z0-9-]*$ ]] || die "lane must be lowercase and dashed: $lane"

    mkdir -p "$VERIFY_DIR"
    local log="$VERIFY_DIR/$lane.log"
    local receipt="$VERIFY_DIR/$lane.json"

    # The scope is whatever `just _changed` selected, read back from the recipe
    # itself: resolving the base again here would be a second, drifting
    # definition of what the run covered.
    local errors scope base_ref
    errors=$(mktemp)
    # shellcheck disable=SC2064 # expand the path now, while it is still set.
    trap "rm -f '$errors'" EXIT
    if ! scope=$(just _changed "$base" 2>"$errors"); then
        cat "$errors" >&2
        die "cannot resolve the changed-file selection"
    fi
    base_ref=$(sed -n 's/^base: //p' "$errors" | tail -n 1)
    [[ -n $base_ref ]] || die "just _changed did not report the base it picked"

    local base_head merge_base
    base_head=$(git rev-parse --verify --quiet "$base_ref^{commit}" || echo unknown)
    merge_base=$(git merge-base "$base_ref" HEAD 2>/dev/null || echo unknown)

    # An override makes the thing under test something this checkout did not
    # necessarily build, whatever the lane would otherwise prove.
    local binaries
    binaries=$(binary_records)
    if [[ $binaries != "[]" && $kind != hardware ]]; then
        kind=binary
    fi

    # Identity is captured before the command runs, so a lane that dirties the
    # tree is described by the source it was handed, not by what it left behind.
    local head branch staged unstaged untracked
    head=$(git rev-parse HEAD)
    branch=$(git branch --show-current)
    staged=$(git diff --cached --name-only | wc -l | tr -d ' ')
    unstaged=$(git diff --name-only | wc -l | tr -d ' ')
    untracked=$(git ls-files --others --exclude-standard | wc -l | tr -d ' ')

    local before after started finished status=0 elapsed
    before=$(source_digest)
    started=$(timestamp)
    elapsed=$SECONDS

    echo "verify: recording $kind evidence for lane '$lane': $*" >&2
    set +e
    "$@" 2>&1 | tee "$log"
    status=${PIPESTATUS[0]}
    set -e

    finished=$(timestamp)
    elapsed=$((SECONDS - elapsed))
    after=$(source_digest)

    # Again, because a long lane gives `just verify clean` in another shell time
    # to remove the directory, and losing the receipt would be the worst moment.
    mkdir -p "$VERIFY_DIR"
    jq -n \
        --arg lane "$lane" \
        --arg kind "$kind" \
        --argjson command "$(jq -n '$ARGS.positional' --args -- "$@")" \
        --argjson exit_status "$status" \
        --arg started "$started" \
        --arg finished "$finished" \
        --argjson finished_epoch "$(date -u +%s)" \
        --argjson elapsed "$elapsed" \
        --arg log "${log#"$ROOT"/}" \
        --arg head "$head" \
        --arg branch "$branch" \
        --arg base "$base_ref" \
        --arg base_head "$base_head" \
        --arg merge_base "$merge_base" \
        --arg digest "$before" \
        --arg digest_after "$after" \
        --argjson staged "$staged" \
        --argjson unstaged "$unstaged" \
        --argjson untracked "$untracked" \
        --argjson scope "$(printf '%s' "$scope" | jq -Rs 'split("\n") | map(select(length > 0))')" \
        --argjson binaries "$binaries" \
        --argjson environment "$(environment_record)" \
        '{
            schema: 1,
            lane: $lane,
            kind: $kind,
            command: $command,
            exit_status: $exit_status,
            started: $started,
            finished: $finished,
            finished_epoch: $finished_epoch,
            elapsed_seconds: $elapsed,
            log: $log,
            source: {
                head: $head,
                branch: $branch,
                base: $base,
                base_head: $base_head,
                merge_base: $merge_base,
                digest: $digest,
                digest_after: $digest_after,
                dirty: {staged: $staged, unstaged: $unstaged, untracked: $untracked},
                scope_count: ($scope | length),
                scope: $scope[0:100]
            },
            binaries: $binaries,
            environment: $environment
        }' >"$receipt"

    current_state
    local graded verdict
    graded=$(grade_receipt "$receipt")
    verdict=$(jq -r .verdict <<<"$graded")
    printf 'verify: %s %s (%s)\n' "$lane" "$verdict" "$(jq -r .reason <<<"$graded")" >&2

    # The command's own status, except that a tree which moved underneath it
    # leaves no usable result either way.
    if ((status != 0)); then
        exit "$status"
    fi
    if [[ $verdict == mixed ]]; then
        exit 1
    fi
}

# Current facts every stored receipt is graded against, read once per command.
current_state() {
    CURRENT_HEAD=$(git rev-parse HEAD)
    CURRENT_DIGEST=$(source_digest)
}

# What the binaries a receipt names hash to right now. An override lives outside
# the source digest, so rebuilding or deleting one leaves the tree untouched and
# the receipt describing something that is no longer there.
binary_digests() {
    local file=$1 path digests='[]'
    while IFS= read -r path; do
        [[ -n $path ]] || continue
        digests=$(jq --arg path "$path" --arg digest "$(blob "$path")" \
            '. + [{path: $path, digest: $digest}]' <<<"$digests")
    done < <(jq -r '.binaries[]?.path // empty' "$file")
    printf '%s' "$digests"
}

# Grades one receipt against the tree as it is now. A pass is a pass only while
# the source, HEAD, and target base it names are the ones in front of you.
grade_receipt() {
    local file=$1 base base_head

    # A receipt that cannot be read is reported as one, never skipped: dropping
    # it would turn a truncated or half-written file into "nothing was claimed",
    # which is the one answer a wrapper like this must never give by accident.
    # Every field the grading filter dereferences is checked, not just a couple:
    # a receipt from an older schema would otherwise abort jq mid-array and be
    # dropped from a list whose whole point is being exhaustive.
    if ! jq -e 'has("lane") and has("exit_status") and has("finished_epoch")
        and (.binaries | type == "array")
        and (.source | type == "object" and has("head") and has("base")
            and has("base_head") and has("digest") and has("digest_after"))' \
        >/dev/null 2>&1 <"$file"; then
        jq -n --arg lane "$(basename -- "${file%.json}")" \
            '{lane: $lane, kind: "unknown", verdict: "unreadable",
              reason: "the receipt is not readable; delete it and run the lane again",
              age_seconds: 0}'
        return
    fi

    base=$(jq -r '.source.base' "$file")
    base_head=$(git rev-parse --verify --quiet "$base^{commit}" || echo unknown)

    jq \
        --arg head "$CURRENT_HEAD" \
        --arg digest "$CURRENT_DIGEST" \
        --arg base_head "$base_head" \
        --argjson binaries "$(binary_digests "$file")" \
        --argjson now "$(date -u +%s)" \
        '. as $receipt
        | (if .exit_status != 0 then
                {verdict: "fail", reason: ("exit status " + (.exit_status | tostring))}
            elif .source.digest != .source.digest_after then
                {verdict: "mixed", reason: "the source changed while the command ran"}
            elif .source.head != $head then
                {verdict: "stale", reason: ("recorded HEAD " + .source.head[0:12] + " is no longer HEAD")}
            elif .source.digest != $digest then
                {verdict: "stale", reason: "the working tree changed after the run"}
            elif .source.base_head != $base_head then
                {verdict: "stale", reason: (.source.base + " moved after the run")}
            elif ([.binaries[] as $recorded | $binaries[]
                    | select(.path == $recorded.path and .digest != $recorded.digest)]
                    | length) > 0 then
                {verdict: "stale", reason: "an overridden binary changed after the run"}
            elif ([.binaries[] | select(.provenance != "checkout")] | length) > 0 then
                {verdict: "exploratory", reason: "an externally supplied binary was under test"}
            else
                {verdict: "pass", reason: "covers the current source"}
            end) as $grade
        | $receipt + $grade + {age_seconds: ($now - .finished_epoch)}' \
        "$file"
}

age() {
    local seconds=$1
    if ((seconds < 90)); then
        printf '%ds' "$seconds"
    elif ((seconds < 5400)); then
        printf '%dm' "$((seconds / 60))"
    elif ((seconds < 172800)); then
        printf '%dh' "$((seconds / 3600))"
    else
        printf '%dd' "$((seconds / 86400))"
    fi
}

# Every stored receipt, graded, so the table and the report join the same data.
graded_receipts() {
    current_state
    local files=() file
    while IFS= read -r file; do
        files+=("$file")
    done < <(find "$VERIFY_DIR" -maxdepth 1 -name '*.json' 2>/dev/null | sort)

    if ((${#files[@]} == 0)); then
        echo '[]'
        return
    fi

    for file in "${files[@]}"; do
        grade_receipt "$file"
    done | jq -s 'sort_by(.lane)'
}

cmd_status() {
    local json=0
    if [[ ${1:-} == --json ]]; then
        json=1
    fi

    local receipts
    receipts=$(graded_receipts)

    if ((json)); then
        printf '%s\n' "$receipts"
    elif [[ $receipts == "[]" ]]; then
        echo "no receipts recorded; run 'just verify check' or 'just verify test'"
    else
        printf '%-12s %-9s %-12s %-6s %s\n' LANE KIND VERDICT AGE DETAIL
        jq -r '.[] | [.lane, .kind, .verdict, (.age_seconds | tostring), .reason] | @tsv' <<<"$receipts" |
            while IFS=$'\t' read -r lane kind verdict seconds reason; do
                printf '%-12s %-9s %-12s %-6s %s\n' "$lane" "$kind" "$verdict" "$(age "$seconds")" "$reason"
            done
    fi

    # Anything but a current pass is unusable as merge evidence, including a run
    # whose binary this checkout cannot account for.
    jq -e 'length > 0 and all(.[]; .verdict == "pass")' >/dev/null <<<"$receipts"
}

# Reads what GitHub currently says, and nothing else: no reruns, no merges, no
# writes. Every fetch is keyed to the pull request's live head, so the report
# cannot describe a commit that has already been superseded.
cmd_pr() {
    need gh

    local number='' json=0 argument
    for argument in "$@"; do
        case $argument in
            --json) json=1 ;;
            -*) usage ;;
            *) number=$argument ;;
        esac
    done

    local fields repo pull
    fields=number,title,url,state,isDraft,headRefName,headRefOid,baseRefName
    fields=$fields,mergeable,mergeStateStatus,reviewDecision
    repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
    # shellcheck disable=SC2086 # an empty number means "the current branch".
    pull=$(gh pr view $number --json "$fields") || die "no pull request found (pass a number)"

    local head base
    head=$(jq -r .headRefOid <<<"$pull")
    base=$(jq -r .baseRefName <<<"$pull")

    # `compare` answers the freshness question the rulesets do not: how far the
    # tested head is behind the branch it would merge into.
    local compare rules checks statuses
    compare=$(gh api "repos/$repo/compare/$base...$head" --jq '{behind_by, ahead_by, status}' 2>/dev/null) ||
        compare=null

    # Effective rules for the branch, which is what actually gates the merge.
    # Workflow YAML does not establish it, and the classic protection endpoint
    # answers "not protected" for a branch that a ruleset covers.
    rules=$(gh api "repos/$repo/rules/branches/$base" 2>/dev/null) || rules=null

    checks=$(gh api "repos/$repo/commits/$head/check-runs?per_page=100" \
        --jq '[.check_runs[] | {id, name, status, conclusion, started_at, html_url, app: .app.id}]' \
        2>/dev/null) ||
        checks=null
    # Commit statuses predate check runs and spell the same thing differently:
    # one `state` field covering both "has it finished" and "did it pass".
    statuses=$(gh api "repos/$repo/commits/$head/status?per_page=100" --jq \
        '[.statuses[] | {id, name: .context, conclusion: .state,
            status: (if .state == "pending" then "in_progress" else "completed" end),
            started_at: .created_at, html_url: .target_url}]' 2>/dev/null) || statuses=null

    local report
    report=$(jq -n \
        --arg repo "$repo" \
        --argjson pr "$pull" \
        --argjson compare "$compare" \
        --argjson rules "$rules" \
        --argjson checks "$checks" \
        --argjson statuses "$statuses" \
        --argjson receipts "$(graded_receipts)" \
        '{repo: $repo, pr: $pr, compare: $compare, rules: $rules,
          checks: $checks, statuses: $statuses, receipts: $receipts}' | cmd_classify)

    if ((json)); then
        printf '%s\n' "$report"
    else
        print_report <<<"$report"
    fi

    [[ $(jq -r .verdict <<<"$report") == green ]]
}

# Pure grading, split from the fetch so the classification of a missing,
# cancelled, or skipped required result is testable without a network.
cmd_classify() {
    jq '
        # A rerun repeats the name. Ordering by id as well as start time matters
        # because a queued attempt can carry no start time at all, and sorting it
        # to the front would hand the grade back to the attempt it replaced.
        # A gate can also pin the app that has to report it, in which case a
        # same-named run from anything else is not the required result.
        def attempts($runs; $gate): $runs
            | map(select(.name == $gate.context
                and (($gate.integration_id // null) == null or .app == $gate.integration_id)))
            | sort_by([(.started_at // ""), (.id // 0)]);

        # A result is green only when it says so. Everything else -- absent,
        # unfinished, cancelled, skipped, neutral -- is its own state. Any
        # unfinished attempt makes the context pending, whatever an older one
        # concluded: a rerun in flight is a result nobody has yet.
        def state($runs; $gate):
            attempts($runs; $gate) as $tries
            | if ($tries | length) == 0 then "missing"
            elif ($tries | map(.status != "completed") | any) then "pending"
            else ($tries | last | .conclusion
                | if . == "success" then "pass"
                elif . == "skipped" then "skipped"
                elif . == "neutral" or . == null then "unknown"
                else "fail"
                end)
            end;

        . as $input
        | (.rules // []) as $rules
        | ([(.checks // []), (.statuses // [])] | add) as $runs
        | ($rules | map(select(.type == "required_status_checks"))) as $gates
        | ($gates | map(.parameters.required_status_checks[])
            | map({context, integration_id: (.integration_id // null)}) | unique) as $gated
        | ($gated | map(.context) | unique) as $required
        | ($gates | map(.parameters.strict_required_status_checks_policy == true) | any) as $strict
        | (($rules | map(select(.type == "merge_queue")) | length) > 0) as $queue
        | ($gated | map({
            context: .context,
            state: state($runs; .),
            url: (attempts($runs; .) | last | if . == null then null else .html_url end)
          })) as $graded
        # Grouped by name for the same reason: a lane that failed and was rerun
        # green must not keep failing the report on its first attempt.
        | ($runs | map(.name) | unique
            | map(select(. as $name | $required | index($name) | not))
            | map({context: ., integration_id: null})
            | map({
                context: .context,
                state: state($runs; .),
                url: (attempts($runs; .) | last | .html_url)
              })) as $extra
        | .pr.headRefOid as $head
        | ((.receipts // []) | map({
            lane: .lane,
            kind: .kind,
            verdict: .verdict,
            state: (if .source.head != $head then "different-head"
                    elif .verdict == "pass" then "current"
                    else .verdict end)
          })) as $evidence
        | [
            (if $input.rules == null then "branch policy could not be read" else empty end),
            (if $input.checks == null then "check runs could not be read" else empty end),
            (if $input.statuses == null then "commit statuses could not be read" else empty end),
            (if $input.compare == null then "the base comparison could not be read" else empty end),
            (if ($required | length) == 0 and $input.rules != null
                then "the base branch requires no status check" else empty end),
            ($graded[] | select(.state != "pass") | "required " + .context + " is " + .state),
            ($extra[] | select(.state == "fail") | "selected lane " + .context + " failed"),
            ($extra[] | select(.state == "pending") | "selected lane " + .context + " has not finished"),
            (if $input.pr.state != "OPEN"
                then "the pull request is " + ($input.pr.state | ascii_downcase) else empty end),
            (if $input.pr.isDraft then "the pull request is a draft" else empty end),
            (if $input.pr.mergeable == "CONFLICTING"
                then "the pull request conflicts with its base" else empty end),
            (if $input.pr.mergeable == "UNKNOWN"
                then "GitHub has not computed mergeability yet; ask again" else empty end),
            (if $input.pr.reviewDecision == "CHANGES_REQUESTED"
                then "a reviewer requested changes" else empty end),
            (if $input.pr.reviewDecision == "REVIEW_REQUIRED"
                then "a required review is missing" else empty end),
            (if ($input.compare.behind_by // 0) > 0 then
                "the head is " + ($input.compare.behind_by | tostring)
                + " commits behind " + $input.pr.baseRefName
                + (if $strict then "" else ", and no rule requires it to be current" end)
             else empty end),
            (if (["CLEAN", "HAS_HOOKS"] | index($input.pr.mergeStateStatus)) == null
                then "GitHub reports the merge state as "
                    + ($input.pr.mergeStateStatus // "unknown") else empty end),
            (if ($evidence | length) == 0
                then "no local receipt; this record is the hosted results only" else empty end),
            ($evidence[] | select(.state != "current") | "local " + .lane + " evidence is " + .state)
          ] as $reasons
        | (if ($graded | map(.state) | any(. == "fail"))
                or ($extra | map(.state) | any(. == "fail"))
                or $input.pr.mergeable == "CONFLICTING"
                or $input.pr.reviewDecision == "CHANGES_REQUESTED"
                or $input.pr.state != "OPEN" then "failed"
            elif ($graded | map(.state) | any(. == "missing" or . == "skipped" or . == "unknown"))
                or $input.rules == null or $input.checks == null or $input.statuses == null
                or $input.compare == null
                or ($required | length) == 0
                or $input.pr.isDraft
                or $input.pr.reviewDecision == "REVIEW_REQUIRED" then "incomplete"
            elif ($graded | map(.state) | any(. == "pending"))
                or ($extra | map(.state) | any(. == "pending"))
                or $input.pr.mergeable == "UNKNOWN" then "pending"
            # Having no local receipt is not staleness: a required result that
            # passed on this exact head is stronger evidence than a local run.
            # A receipt describing another head is, because it invites a reader
            # to credit this candidate with what a different one proved.
            elif ($input.compare.behind_by // 0) > 0
                or ($evidence | map(.state) | any(. != "current")) then "stale"
            # Last, because every state above explains itself. If nothing here
            # is outstanding and GitHub still will not call the branch mergeable,
            # something is gating the merge that this report cannot see, and a
            # green verdict would be claiming otherwise.
            elif (["CLEAN", "HAS_HOOKS"] | index($input.pr.mergeStateStatus)) == null then "incomplete"
            else "green"
            end) as $verdict
        | {
            verdict: $verdict,
            reasons: $reasons,
            pr: {
                number: .pr.number,
                title: .pr.title,
                url: .pr.url,
                head: $head,
                base: .pr.baseRefName,
                state: .pr.state,
                draft: .pr.isDraft,
                mergeable: .pr.mergeable,
                merge_state: .pr.mergeStateStatus,
                review: .pr.reviewDecision
            },
            policy: {required: $required, strict: $strict, merge_queue: $queue},
            freshness: {behind_by: (.compare.behind_by // null), ahead_by: (.compare.ahead_by // null)},
            required: $graded,
            extra: $extra,
            evidence: $evidence
        }'
}

print_report() {
    jq -r '
        "pull request  #\(.pr.number) \(.pr.title)",
        "              \(.pr.url)",
        "head          \(.pr.head[0:12]) -> \(.pr.base)"
            + "   behind: \(.freshness.behind_by // "?")   ahead: \(.freshness.ahead_by // "?")",
        "mergeable     \(.pr.mergeable) / \(.pr.merge_state)"
            + "   review: \(if (.pr.review // "") == "" then "none" else .pr.review end)",
        "policy        required: \(.policy.required | join(", ") | if . == "" then "none" else . end)"
            + "   strict: \(.policy.strict)   merge queue: \(.policy.merge_queue)",
        "",
        "REQUIRED",
        (.required[] | "  \(.state | ascii_upcase)  \(.context)"),
        (if (.extra | length) > 0 then "", "OTHER CHECKS" else empty end),
        (.extra[] | "  \(.state | ascii_upcase)  \(.context)"),
        (if (.evidence | length) > 0 then "", "LOCAL EVIDENCE" else empty end),
        (.evidence[] | "  \(.state | ascii_upcase)  \(.lane) (\(.kind))"),
        "",
        "verdict       \(.verdict)",
        (.reasons[] | "  - \(.)")
    '
}

# The compact record: what this checkout proved and what the merge candidate
# says, in one place. Evidence only. Merging stays a separate action that has to
# recheck the head against GitHub's own gate.
cmd_report() {
    local json=0 arguments=() argument
    for argument in "$@"; do
        case $argument in
            --json) json=1 ;;
            *) arguments+=("$argument") ;;
        esac
    done

    # One verdict decides the exit status in both forms. `cmd_pr` already grades
    # the local receipts into its own evidence, so folding in `cmd_status` here
    # would make the text form disagree with the JSON one about the same PR.
    local status=0
    if ((json)); then
        local receipts pull
        receipts=$(graded_receipts)
        pull=$(cmd_pr ${arguments[0]:+"${arguments[0]}"} --json) || status=$?
        jq -n --argjson receipts "$receipts" --argjson pull "${pull:-null}" \
            '{receipts: $receipts, pull_request: $pull}'
    else
        echo "LOCAL RECEIPTS"
        cmd_status || true
        echo
        cmd_pr ${arguments[0]:+"${arguments[0]}"} || status=$?
    fi
    return "$status"
}

case "${1:-}" in
    record)
        shift
        cmd_record "$@"
        ;;
    status)
        shift
        cmd_status "$@"
        ;;
    pr)
        shift
        cmd_pr "$@"
        ;;
    report)
        shift
        cmd_report "$@"
        ;;
    classify)
        shift
        cmd_classify "$@"
        ;;
    *) usage ;;
esac
