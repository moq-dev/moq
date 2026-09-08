#!/usr/bin/env bash
#
# Regression test for verify.sh, the thing whose whole job is to notice that a
# recorded pass no longer applies. A wrapper that always says "pass" would look
# exactly like a working one, so every invalidation is exercised here.
#
# Runs against a throwaway repository rather than this checkout: the cases have
# to move HEAD, dirty the tree, and advance the target branch, and none of that
# belongs in the caller's worktree. `just _changed` is stubbed, because the
# selection it performs is the root justfile's own tested concern.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERIFY="$SCRIPT_DIR/verify.sh"

fail() {
    echo "verify: verify.test.sh: $1" >&2
    exit 1
}

FIXTURE=$(mktemp -d)
trap 'rm -rf "$FIXTURE"' EXIT

run() {
    (cd "$FIXTURE" && PATH="$FIXTURE/bin:$PATH" "$VERIFY" "$@")
}

# `run` with extra environment for that one invocation, given as leading
# NAME=VALUE arguments. `env` rather than `export`, so a binary override cannot
# leak into the case after it.
run_with() {
    local environment=()
    while [[ ${1-} == *=* ]]; do
        environment+=("$1")
        shift
    done
    (cd "$FIXTURE" && PATH="$FIXTURE/bin:$PATH" env "${environment[@]}" "$VERIFY" "$@")
}

# The verdict `status` currently assigns to the only stored receipt.
verdict() {
    run status --json | jq -r '.[0].verdict'
}

git -C "$FIXTURE" init --quiet -b work
git -C "$FIXTURE" config user.email verify@example.com
git -C "$FIXTURE" config user.name verify
# The fixture commits constantly; a globally configured signing key would ask
# for a passphrase in the middle of `just check`.
git -C "$FIXTURE" config commit.gpgsign false

mkdir -p "$FIXTURE/bin"

# Stands in for the root justfile's `_changed`: reports the base it picked on
# stderr and the changed files on stdout, untracked ones included.
write_stub() {
    cat >"$FIXTURE/bin/just" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ ${1:-} == _changed ]] || {
    echo "unexpected just invocation: $*" >&2
    exit 2
}
echo "base: base" >&2
git diff --name-only "$(git merge-base base HEAD)"
git ls-files --others --exclude-standard
EOF
    chmod +x "$FIXTURE/bin/just"
}
write_stub

printf 'bin/\n.verify/\n' >"$FIXTURE/.gitignore"
printf 'one\n' >"$FIXTURE/tracked.txt"
printf '# docs\n' >"$FIXTURE/docs.md"
git -C "$FIXTURE" add .gitignore tracked.txt docs.md
git -C "$FIXTURE" commit --quiet -m "initial"
git -C "$FIXTURE" branch base

# A pass covers the source it ran against, and says so.
run record local demo "" -- true >/dev/null 2>&1 || fail "a passing command must record a pass"
[[ "$(verdict)" == pass ]] || fail "a fresh receipt must be a pass"
run status >/dev/null || fail "a fresh receipt must exit zero"

# ...and stops covering it the moment a tracked file changes.
printf 'two\n' >>"$FIXTURE/tracked.txt"
[[ "$(verdict)" == stale ]] || fail "a tracked edit must invalidate the receipt"
! run status >/dev/null 2>&1 || fail "a stale receipt must exit nonzero"
git -C "$FIXTURE" checkout --quiet -- tracked.txt
[[ "$(verdict)" == pass ]] || fail "reverting the edit must restore the receipt"

# An untracked fixture is source too: a new file is often the whole change.
printf 'payload\n' >"$FIXTURE/fixture.bin"
[[ "$(verdict)" == stale ]] || fail "an untracked file must invalidate the receipt"
rm "$FIXTURE/fixture.bin"
[[ "$(verdict)" == pass ]] || fail "removing the untracked file must restore the receipt"

# A new commit is a different candidate, even with an identical tree.
git -C "$FIXTURE" commit --quiet --allow-empty -m "second"
[[ "$(verdict)" == stale ]] || fail "a new HEAD must invalidate the receipt"

# The target branch moving means the tested merge result no longer exists.
run record local demo "" -- true >/dev/null 2>&1 || fail "re-recording must succeed"
git -C "$FIXTURE" checkout --quiet base
git -C "$FIXTURE" commit --quiet --allow-empty -m "base moves"
git -C "$FIXTURE" checkout --quiet work
[[ "$(verdict)" == stale ]] || fail "the base moving must invalidate the receipt"

# A run whose own source changed underneath it proved nothing about either tree.
! run record local demo "" -- sh -c 'printf x > mixed.txt' >/dev/null 2>&1 ||
    fail "a run that changed the source must exit nonzero"
[[ "$(verdict)" == mixed ]] || fail "a source change during the run must be mixed"
rm "$FIXTURE/mixed.txt"

# A failing command is a failing receipt, and keeps its exit status.
! run record local demo "" -- sh -c 'exit 3' >/dev/null 2>&1 || fail "a failing command must exit nonzero"
[[ "$(verdict)" == fail ]] || fail "a failing command must record a failure"

# A binary this checkout cannot account for is exploratory, never a pass.
run_with RELAY_BIN="$FIXTURE/bin/just" record local demo "" -- true >/dev/null 2>&1 ||
    fail "an overridden binary must still record"
[[ "$(verdict)" == exploratory ]] || fail "an unknown binary must be exploratory"
! run status >/dev/null 2>&1 || fail "exploratory evidence must not exit zero"
[[ "$(run status --json | jq -r '.[0].kind')" == binary ]] || fail "an override must relabel the kind"
[[ "$(run status --json | jq -r '.[0].binaries[0].provenance')" == external ]] ||
    fail "an override outside the target directory must be external"
[[ "$(run status --json | jq -r '.[0].binaries[0].digest')" =~ ^[0-9a-f]{40}$ ]] ||
    fail "an override must be digested"

# ...and the digest is there to be checked. An override lives outside the source
# digest, so rebuilding one leaves the tree identical and the receipt wrong.
printf '#!/usr/bin/env bash\nexit 0\n' >"$FIXTURE/bin/just"
[[ "$(verdict)" == stale ]] || fail "a changed override must invalidate the receipt"
rm "$FIXTURE/bin/just"
[[ "$(verdict)" == stale ]] || fail "a deleted override must invalidate the receipt"
write_stub

# Provenance follows the leaf, not just its directory: a symlink parked in the
# target directory and pointing somewhere else is still someone else's binary,
# and resolving only the directory would credit this checkout with it.
rm -rf "${FIXTURE:?}/.verify"
mkdir -p "$FIXTURE/target/debug" "$FIXTURE/elsewhere"
printf '#!/usr/bin/env bash\nexit 0\n' >"$FIXTURE/elsewhere/relay"
ln -s "$FIXTURE/elsewhere/relay" "$FIXTURE/target/debug/relay"
run_with CARGO_TARGET_DIR="$FIXTURE/target" RELAY_BIN="$FIXTURE/target/debug/relay" \
    record local demo "" -- true >/dev/null 2>&1 || fail "a symlinked override must record"
[[ "$(run status --json | jq -r '.[0].binaries[0].provenance')" == external ]] ||
    fail "a symlink out of the target directory must be external"
[[ "$(verdict)" == exploratory ]] || fail "a symlinked external binary must be exploratory"

# ...while a binary that really is in there is the checkout's own.
cp "$FIXTURE/elsewhere/relay" "$FIXTURE/target/debug/own"
run_with CARGO_TARGET_DIR="$FIXTURE/target" RELAY_BIN="$FIXTURE/target/debug/own" \
    record local demo "" -- true >/dev/null 2>&1 || fail "a checkout override must record"
[[ "$(run status --json | jq -r '.[0].binaries[0].provenance')" == checkout ]] ||
    fail "a binary inside the target directory must be the checkout's"
[[ "$(verdict)" == pass ]] || fail "a binary built here must not be exploratory"
rm -rf "${FIXTURE:?}/target" "${FIXTURE:?}/elsewhere"

# A docs-only change is reported at its own size: the wrapper adds no work.
rm -rf "${FIXTURE:?}/.verify"
printf '# docs\nmore\n' >"$FIXTURE/docs.md"
run record static docs "" -- true >/dev/null 2>&1 || fail "a docs-only run must record"
[[ "$(run status --json | jq -r '.[0].source.scope | join(",")')" == docs.md ]] ||
    fail "the receipt must record the selected scope"
[[ "$(run status --json | jq -r '.[0].command | join(" ")')" == true ]] ||
    fail "the receipt must record the command it ran"

# An option-shaped argument belongs to the command, not to whatever the recorder
# writes the receipt with. Getting this wrong produced an empty receipt.
run record static docs "" -- true --show check >/dev/null 2>&1 ||
    fail "an option-shaped argument must not break recording"
[[ "$(run status --json | jq -r '.[0].command | join(" ")')" == "true --show check" ]] ||
    fail "the receipt must record option-shaped arguments verbatim"

# A receipt that cannot be read is a missing result, and has to say so rather
# than vanish from a list that is supposed to be exhaustive.
: >"$FIXTURE/.verify/docs.json"
[[ "$(verdict)" == unreadable ]] || fail "an unreadable receipt must be reported"
! run status >/dev/null 2>&1 || fail "an unreadable receipt must exit nonzero"

# Valid JSON missing a field the grading reads is the same problem: jq would
# abort halfway and the receipt would disappear from an exhaustive list.
echo '{"lane": "docs", "source": {}}' >"$FIXTURE/.verify/docs.json"
[[ "$(verdict)" == unreadable ]] || fail "a partial receipt must be reported, not dropped"

# Every key present with the wrong type is the same problem one step later: the
# age arithmetic aborts jq, and an exhaustive list loses an entry.
rm -rf "${FIXTURE:?}/.verify"
run record static docs "" -- true >/dev/null 2>&1 || fail "recording must succeed"
jq '.finished_epoch = "recently"' "$FIXTURE/.verify/docs.json" >"$FIXTURE/.verify/docs.tmp"
mv "$FIXTURE/.verify/docs.tmp" "$FIXTURE/.verify/docs.json"
[[ "$(verdict)" == unreadable ]] || fail "a wrongly typed receipt must be reported, not dropped"

# Everything below grades gathered pull request state, with no network: a
# required result that is absent, cancelled, skipped, or unfinished must never
# read as green, and neither must evidence recorded against another head.
green=$(
    jq -n '{
        repo: "moq-dev/moq",
        pr: {
            number: 1, title: "t", url: "u", state: "OPEN", isDraft: false,
            headRefName: "work", headRefOid: "head1", baseRefName: "main",
            mergeable: "MERGEABLE", mergeStateStatus: "CLEAN", reviewDecision: null
        },
        compare: {behind_by: 0, ahead_by: 1, status: "ahead"},
        rules: [{
            type: "required_status_checks",
            parameters: {
                strict_required_status_checks_policy: false,
                required_status_checks: [{context: "Check"}, {context: "Test"}]
            }
        }],
        checks: [
            {name: "Check", status: "completed", conclusion: "success", started_at: "1"},
            {name: "Test", status: "completed", conclusion: "success", started_at: "1"}
        ],
        statuses: [],
        receipts: [{lane: "check", kind: "static", verdict: "pass",
            source: {head: "head1", base: "origin/main"}}]
    }'
)

grade() {
    jq "$1" <<<"$green" | "$VERIFY" classify | jq -r .verdict
}

[[ "$(grade '.')" == green ]] || fail "a complete, current candidate must be green"
[[ "$(grade '.checks |= map(select(.name != "Test"))')" == incomplete ]] ||
    fail "a required job that never ran must block"
[[ "$(grade '.checks[1].conclusion = "cancelled"')" == failed ]] ||
    fail "a cancelled required run must block"
[[ "$(grade '.checks[1].conclusion = "skipped"')" == incomplete ]] ||
    fail "a skipped required run must block"
[[ "$(grade '.checks[1].conclusion = "neutral"')" == incomplete ]] ||
    fail "an inconclusive required run must block"
[[ "$(grade '.checks[1] |= (.status = "in_progress" | .conclusion = null)')" == pending ]] ||
    fail "an unfinished required run must not be green"
failed_lane='.checks += [{name: "Smoke", status: "completed", conclusion: "failure", started_at: "1"}]'
[[ "$(grade "$failed_lane")" == failed ]] || fail "a failed extra lane must block"
[[ "$(grade '.compare.behind_by = 3')" == stale ]] || fail "a head behind its base must be stale"
[[ "$(grade '.rules = null')" == incomplete ]] || fail "unreadable branch policy must block"
[[ "$(grade '.rules = []')" == incomplete ]] || fail "a base requiring nothing must block"
[[ "$(grade '.receipts = []')" == green ]] ||
    fail "a required result on this head must stand without a local receipt"
[[ "$(grade '.receipts[0].source.head = "other"')" == stale ]] ||
    fail "evidence from another head must be stale"

# Retargeting a pull request leaves the head alone and changes what the
# candidate merges into, so the scope every local lane ran was picked against a
# base that is no longer the one in question.
[[ "$(grade '.receipts[0].source.base = "origin/dev"')" == stale ]] ||
    fail "evidence recorded against another base must be stale"
[[ "$(grade '.receipts[0].source.base = "main"')" == green ]] ||
    fail "a base recorded without its remote must still match"
[[ "$(grade '.pr.mergeable = "CONFLICTING"')" == failed ]] || fail "a conflicting candidate must block"
[[ "$(grade '.pr.reviewDecision = "CHANGES_REQUESTED"')" == failed ]] || fail "requested changes must block"
[[ "$(grade '.pr.isDraft = true')" == incomplete ]] || fail "a draft must not be green"

# A rerun repeats the name, and the newest attempt is the one that counts.
rerun='.checks[0].conclusion = "failure"
    | .checks += [{name: "Check", id: 2, status: "completed", conclusion: "success", started_at: "2"}]'
[[ "$(grade "$rerun")" == green ]] || fail "the newest attempt of a required job must win"

# A queued rerun can arrive with no start time at all. Sorting it to the front
# would hand the grade back to the attempt it is replacing.
queued='.checks += [{name: "Check", id: 9, status: "queued", conclusion: null, started_at: null}]'
[[ "$(grade "$queued")" == pending ]] || fail "a queued rerun must not inherit the old pass"

# The same rule has to hold for the lanes policy does not require, or a lane
# that failed and was rerun green keeps failing the whole report.
extra_rerun='.checks += [
    {name: "Smoke", id: 3, status: "completed", conclusion: "failure", started_at: "1"},
    {name: "Smoke", id: 4, status: "completed", conclusion: "success", started_at: "2"}]'
[[ "$(grade "$extra_rerun")" == green ]] || fail "a rerun extra lane must be graded on its newest attempt"

# A gate can pin the app that has to report it, and a same-named run from
# anything else is then not the required result.
pinned='.rules[0].parameters.required_status_checks[0].integration_id = 15368
    | .checks[0].app = 15368 | .checks[1].app = 15368'
[[ "$(grade "$pinned")" == green ]] || fail "the pinned app reporting the check must pass"
[[ "$(grade "$pinned | .checks[0].app = 99")" == incomplete ]] ||
    fail "a same-named check from another app must not satisfy a pinned gate"

# A feed that could not be read is not an empty feed.
[[ "$(grade '.statuses = null')" == incomplete ]] || fail "unreadable commit statuses must block"

# Everything known can be clean while GitHub still refuses the merge, and the
# one verdict that claims readiness must not be the one that hides that.
[[ "$(grade '.pr.mergeStateStatus = "BLOCKED"')" == incomplete ]] ||
    fail "an unexplained merge state must not be green"
[[ "$(grade '.pr.mergeStateStatus = "HAS_HOOKS"')" == green ]] ||
    fail "a merge state that only means hooks must stay green"

# `report` prints the receipts and the pull request together, and one verdict
# has to decide its exit status, or its two forms disagree about the same
# candidate: `status` alone fails when nothing was recorded locally, while a
# required result on this exact head is green with no receipt at all.
cat >"$FIXTURE/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
head=$(git rev-parse HEAD)
case "$1 ${2:-}" in
    "repo view") echo moq-dev/moq ;;
    "pr view")
        jq -n --arg head "$head" '{
            number: 1, title: "t", url: "u", state: "OPEN", isDraft: false,
            headRefName: "work", headRefOid: $head, baseRefName: "main",
            mergeable: "MERGEABLE", mergeStateStatus: "CLEAN", reviewDecision: null}' ;;
    "api "*compare*) echo '{"behind_by": 0, "ahead_by": 1, "status": "ahead"}' ;;
    "api "*rules*)
        echo '[{"type": "required_status_checks", "parameters": {
            "strict_required_status_checks_policy": false,
            "required_status_checks": [{"context": "Check"}]}}]' ;;
    "api "*check-runs*)
        echo '[{"id": 1, "name": "Check", "status": "completed",
            "conclusion": "success", "started_at": "1", "html_url": "u"}]' ;;
    "api "*status*) echo '[]' ;;
    *) echo "unexpected gh invocation: $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$FIXTURE/bin/gh"

rm -rf "${FIXTURE:?}/.verify"
! run status >/dev/null 2>&1 || fail "no local receipt must fail status"
run report >/dev/null 2>&1 || fail "a hosted pass on this head must make report exit zero"
run report --json >/dev/null 2>&1 || fail "both forms of report must agree on the verdict"

# ...and evidence describing a different candidate still fails it, so the
# unified status is not just a status that never fails.
run record local demo "" -- true >/dev/null 2>&1 || fail "recording must succeed"
git -C "$FIXTURE" commit --quiet --allow-empty -m "report moves on"
! run report >/dev/null 2>&1 || fail "a receipt from another head must fail report"

echo "verify: receipt and readiness regression ok"
