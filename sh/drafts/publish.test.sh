#!/usr/bin/env bash
# publish.sh opens the next changelog section only after the datatracker
# accepts the submission. Fixtures stand in for curl and kramdown-rfc.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
publish=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/publish.sh
before=$(sha256sum "$root"/drafts/draft-*.md)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

repo=$tmp/repo
bin=$tmp/bin
mkdir -p "$bin" "$repo/drafts"
git -C "$repo" init -q
if [[ $(git -C "$repo" rev-parse --show-toplevel) != "$repo" ]]; then
    echo "fixture repo is not its own toplevel" >&2
    exit 1
fi

cat >"$bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'curl\n' >>"$CURL_LOG"
out=
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o)
            out=$2
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
if [[ -n ${FAKE_CURL_FAIL:-} ]]; then
    echo "fake curl failed" >&2
    exit 1
fi
if [[ -n $out ]]; then
    printf 'accepted\n' >"$out"
fi
printf '%s' "${FAKE_HTTP_CODE:-200}"
EOF

cat >"$bin/kramdown-rfc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'kramdown\n' >>"$KRAM_LOG"
cat >"$KRAM_STDIN"
printf '<rfc/>\n'
EOF
chmod +x "$bin/curl" "$bin/kramdown-rfc"

export PATH="$bin:$PATH"
export CURL_LOG=$tmp/curl.log
export KRAM_LOG=$tmp/kram.log
export KRAM_STDIN=$tmp/kram.stdin

fail() {
    echo "FAIL: $*" >&2
    if [[ -f $tmp/out ]]; then
        echo "--- stdout ---" >&2
        cat "$tmp/out" >&2
        echo "--- stderr ---" >&2
        cat "$tmp/err" >&2
    fi
    exit 1
}

reset_logs() {
    : >"$CURL_LOG"
    : >"$KRAM_LOG"
    : >"$KRAM_STDIN"
}

install_draft() {
    local name=$1
    local src=$2
    rm -f "$repo/drafts/"*
    cp "$src" "$repo/drafts/$name.md"
}

# run CODE CURL_FAIL NAME VERSION
# Sets RUN_RC. A curl or kramdown stand-in failure is the case under test.
run() {
    local code=$1
    local curl_fail=$2
    shift 2
    reset_logs
    RUN_RC=0
    (
        cd "$repo"
        [[ $(git rev-parse --show-toplevel) == "$repo" ]]
        FAKE_HTTP_CODE=$code FAKE_CURL_FAIL=$curl_fail "$publish" "$@"
    ) >"$tmp/out" 2>"$tmp/err" || RUN_RC=$?
}

unchanged() {
    local src=$1
    local name=$2
    cmp -s "$src" "$repo/drafts/$name.md" || fail "$name was edited"
}

no_submit() {
    [[ ! -s $CURL_LOG ]] || fail "datatracker was contacted"
    [[ ! -s $KRAM_LOG ]] || fail "draft was rendered"
}

# The version on the first changelog heading, the in-progress section.
first_version() {
    local file=$1
    local -a lines=()
    mapfile -t lines <"$file"
    local -i i n=${#lines[@]}
    local -i start=-1
    for ((i = 0; i < n; i++)); do
        if [[ ${lines[$i]} =~ ^#\ .*Changelog[[:space:]]*$ ]]; then
            start=$i
            break
        fi
    done
    ((start >= 0)) || return 1
    for ((i = start + 1; i < n; i++)); do
        if [[ ${lines[$i]} =~ ^#[[:space:]] ]]; then
            echo "$file changelog has no version heading" >&2
            return 1
        fi
        if [[ ${lines[$i]} =~ ^##[[:space:]] ]]; then
            if [[ ${lines[$i]} =~ ^##\ .*-([0-9][0-9])(\ \(in\ progress\))?$ ]]; then
                printf '%s\n' "${BASH_REMATCH[1]}"
                return 0
            fi
            echo "$file first changelog heading is not a version: ${lines[$i]}" >&2
            return 1
        fi
    done
    echo "$file changelog has no version heading" >&2
    return 1
}

assert_opened() {
    local before_file=$1
    local after_file=$2
    local ver=$3
    local next=$4
    local removed
    removed=$(diff -u "$before_file" "$after_file" | grep -E '^-' | grep -vE '^---' || true)
    [[ -z $removed ]] || fail "existing lines changed: $removed"

    local old
    old=$(grep -E "^## .*-${ver}( \\(in progress\\))?$" "$before_file" | head -n 1)
    [[ -n $old ]] || fail "missing published heading for $ver"
    local new=${old/$ver/$next}
    [[ $(grep -cFx "$new" "$after_file") -eq 1 ]] || fail "want one $new heading"
    [[ $(grep -cFx "$old" "$after_file") -eq 1 ]] || fail "published heading $old was rewritten"

    local -a before_lines after_lines
    mapfile -t before_lines <"$before_file"
    mapfile -t after_lines <"$after_file"
    local -i old_idx=-1 i
    for ((i = 0; i < ${#before_lines[@]}; i++)); do
        if [[ ${before_lines[$i]} == "$old" ]]; then
            old_idx=$i
            break
        fi
    done
    ((old_idx >= 0)) || fail "could not find $old"
    local -a attrs=()
    local -i j=$((old_idx + 1))
    while ((j < ${#before_lines[@]})) && [[ ${before_lines[$j]} =~ ^\{: ]]; do
        attrs+=("${before_lines[$j]}")
        j+=1
    done

    local -i new_idx=-1 after_old=-1
    for ((i = 0; i < ${#after_lines[@]}; i++)); do
        if [[ ${after_lines[$i]} == "$new" && new_idx -lt 0 ]]; then
            new_idx=$i
        fi
        if [[ ${after_lines[$i]} == "$old" ]]; then
            after_old=$i
        fi
    done
    ((new_idx >= 0 && after_old > new_idx)) || fail "$new is not above $old"
    local -i expect=$((new_idx + 1))
    if ((${#attrs[@]} > 0)); then
        local attr
        for attr in "${attrs[@]}"; do
            [[ ${after_lines[$expect]} == "$attr" ]] || fail "attribute was not copied onto $new"
            expect+=1
        done
    fi
    [[ -z ${after_lines[$expect]} ]] || fail "next section is not empty"
    expect+=1
    ((expect == after_old)) || fail "unexpected lines between $new and $old"

    local growth=$((${#after_lines[@]} - ${#before_lines[@]}))
    local want=$((2 + ${#attrs[@]}))
    ((growth == want)) || fail "inserted $growth lines, want $want"
}

expect_file() {
    local name=$1
    local src=$2
    local want=$3
    cmp -s "$want" "$repo/drafts/$name.md" || {
        diff -u "$want" "$repo/drafts/$name.md" >&2 || true
        fail "$name did not match the expected changelog"
    }
    # The rendered text is the draft from before the insert.
    cmp -s "$src" "$KRAM_STDIN" || fail "$name was rendered after the insert"
}

cat >"$tmp/hang.md" <<'EOF'
# Appendix A: Changelog
{:numbered="false"}

## moq-hang-04
{:numbered="false"}

- Defined encoder jitter.
EOF
cat >"$tmp/hang-next.md" <<'EOF'
# Appendix A: Changelog
{:numbered="false"}

## moq-hang-05
{:numbered="false"}

## moq-hang-04
{:numbered="false"}

- Defined encoder jitter.
EOF

cat >"$tmp/lite.md" <<'EOF'
# Appendix A: Changelog

## moq-lite-07

- A subscription range bounds datagrams.
EOF
cat >"$tmp/lite-next.md" <<'EOF'
# Appendix A: Changelog

## moq-lite-08

## moq-lite-07

- A subscription range bounds datagrams.
EOF

cat >"$tmp/cluster.md" <<'EOF'
# Appendix A: Changelog

## moq-cluster-02
- Assigned identities stay local.
EOF
cat >"$tmp/cluster-next.md" <<'EOF'
# Appendix A: Changelog

## moq-cluster-03

## moq-cluster-02
- Assigned identities stay local.
EOF

cat >"$tmp/hidden.md" <<'EOF'
# Changelog

## Since draft-lcurley-moq-hidden-00 (in progress)

- Apply hidden filtering only to opted-in peers.
EOF
cat >"$tmp/hidden-next.md" <<'EOF'
# Changelog

## Since draft-lcurley-moq-hidden-01 (in progress)

## Since draft-lcurley-moq-hidden-00 (in progress)

- Apply hidden filtering only to opted-in peers.
EOF

cat >"$tmp/e2ee.md" <<'EOF'
# Changelog
{:numbered="false"}

## draft-lcurley-moq-e2ee-00
{:numbered="false"}

- Initial profile.
EOF
cat >"$tmp/e2ee-next.md" <<'EOF'
# Changelog
{:numbered="false"}

## draft-lcurley-moq-e2ee-01
{:numbered="false"}

## draft-lcurley-moq-e2ee-00
{:numbered="false"}

- Initial profile.
EOF

cat >"$tmp/v08.md" <<'EOF'
# Changelog

## moq-hang-08

- Octal-looking version.
EOF
cat >"$tmp/v09.md" <<'EOF'
# Changelog

## moq-hang-09

## moq-hang-08

- Octal-looking version.
EOF
cat >"$tmp/v10.md" <<'EOF'
# Changelog

## moq-hang-10

## moq-hang-09

## moq-hang-08

- Octal-looking version.
EOF

cat >"$tmp/plain.md" <<'EOF'
# Introduction

No changelog here.
EOF

cat >"$tmp/mismatch.md" <<'EOF'
# Changelog

## moq-hang-04

- In progress.
EOF

cat >"$tmp/below.md" <<'EOF'
# Changelog

## moq-hang-04

- In progress.

## moq-hang-05

- Already opened, but below.
EOF

cat >"$tmp/above.md" <<'EOF'
# Changelog

## moq-hang-05

- Already open.

## moq-hang-04

- Published.
EOF

cat >"$tmp/other.md" <<'EOF'
# Changelog

## moq-lite-01

- Leave this draft alone.
EOF

check_case() {
    local label=$1
    local src=$2
    local name=$3
    local version=$4
    local want=$5
    install_draft "$name" "$src"
    cp "$tmp/other.md" "$repo/drafts/draft-other.md"
    run 200 "" "$name" "$version" test@example.com
    [[ $RUN_RC -eq 0 ]] || fail "$label exited $RUN_RC"
    grep -q "HTTP 200" "$tmp/out" || fail "$label did not report HTTP 200"
    grep -q "Changelog section " "$tmp/out" || fail "$label did not report the open section"
    expect_file "$name" "$src" "$want"
    cmp -s "$tmp/other.md" "$repo/drafts/draft-other.md" || fail "$label edited another draft"
    local once=$tmp/once.md
    cp "$repo/drafts/$name.md" "$once"
    run 201 "" "$name" "$version" test@example.com
    [[ $RUN_RC -eq 0 ]] || fail "$label republish exited $RUN_RC"
    cmp -s "$once" "$repo/drafts/$name.md" || fail "$label republish rewrote the changelog"
}

check_case "hang" "$tmp/hang.md" draft-lcurley-moq-hang 04 "$tmp/hang-next.md"
check_case "lite" "$tmp/lite.md" draft-lcurley-moq-lite 07 "$tmp/lite-next.md"
check_case "cluster" "$tmp/cluster.md" draft-lcurley-moq-cluster 02 "$tmp/cluster-next.md"
check_case "hidden" "$tmp/hidden.md" draft-lcurley-moq-hidden 00 "$tmp/hidden-next.md"
check_case "e2ee" "$tmp/e2ee.md" draft-lcurley-moq-e2ee 00 "$tmp/e2ee-next.md"
check_case "version 08" "$tmp/v08.md" draft-lcurley-moq-hang 08 "$tmp/v09.md"

install_draft draft-lcurley-moq-hang "$tmp/v09.md"
run 200 "" draft-lcurley-moq-hang 09 test@example.com
[[ $RUN_RC -eq 0 ]] || fail "version 09 exited $RUN_RC"
expect_file draft-lcurley-moq-hang "$tmp/v09.md" "$tmp/v10.md"

install_draft draft-lcurley-moq-hang "$tmp/hang.md"
run 400 "" draft-lcurley-moq-hang 04 test@example.com
[[ $RUN_RC -ne 0 ]] || fail "HTTP 400 succeeded"
grep -q "Submission failed" "$tmp/err" || fail "HTTP 400 did not report failure"
unchanged "$tmp/hang.md" draft-lcurley-moq-hang
[[ -s $CURL_LOG ]] || fail "HTTP 400 never contacted the datatracker"

install_draft draft-lcurley-moq-hang "$tmp/hang.md"
run 200 1 draft-lcurley-moq-hang 04 test@example.com
[[ $RUN_RC -ne 0 ]] || fail "curl failure succeeded"
unchanged "$tmp/hang.md" draft-lcurley-moq-hang
[[ -s $KRAM_LOG ]] || fail "curl failure skipped the build"
if grep -q "Submitted" "$tmp/out"; then
    fail "curl failure reported success"
fi

install_draft draft-lcurley-moq-probe "$tmp/plain.md"
run 200 "" draft-lcurley-moq-probe 00 test@example.com
[[ $RUN_RC -ne 0 ]] || fail "missing changelog succeeded"
no_submit
unchanged "$tmp/plain.md" draft-lcurley-moq-probe

install_draft draft-lcurley-moq-hang "$tmp/mismatch.md"
run 200 "" draft-lcurley-moq-hang 03 test@example.com
[[ $RUN_RC -ne 0 ]] || fail "missing published section succeeded"
no_submit
unchanged "$tmp/mismatch.md" draft-lcurley-moq-hang

install_draft draft-lcurley-moq-hang "$tmp/below.md"
run 200 "" draft-lcurley-moq-hang 04 test@example.com
[[ $RUN_RC -ne 0 ]] || fail "next section below the published one succeeded"
no_submit
unchanged "$tmp/below.md" draft-lcurley-moq-hang

install_draft draft-lcurley-moq-hang "$tmp/above.md"
run 200 "" draft-lcurley-moq-hang 04 test@example.com
[[ $RUN_RC -eq 0 ]] || fail "existing next section exited $RUN_RC"
unchanged "$tmp/above.md" draft-lcurley-moq-hang
[[ -s $CURL_LOG ]] || fail "existing next section skipped submission"
cmp -s "$tmp/above.md" "$KRAM_STDIN" || fail "existing next section changed the render"

install_draft draft-lcurley-moq-hang "$tmp/hang.md"
run 200 "" draft-lcurley-moq-hang 99 test@example.com
[[ $RUN_RC -ne 0 ]] || fail "version 99 succeeded"
no_submit
unchanged "$tmp/hang.md" draft-lcurley-moq-hang

for src in "$root"/drafts/draft-*.md; do
    name=$(basename "$src" .md)
    install_draft "$name" "$src"
    if ! grep -qE '^# .*Changelog[[:space:]]*$' "$src"; then
        run 200 "" "$name" 00 test@example.com
        [[ $RUN_RC -ne 0 ]] || fail "$name submitted without a changelog"
        no_submit
        unchanged "$src" "$name"
        continue
    fi
    ver=$(first_version "$src") || fail "$name changelog style is not recognized"
    next=$(printf '%02d' $((10#$ver + 1)))
    run 200 "" "$name" "$ver" test@example.com
    [[ $RUN_RC -eq 0 ]] || fail "$name publish $ver exited $RUN_RC"
    assert_opened "$src" "$repo/drafts/$name.md" "$ver" "$next"
    sed "s/${name}-latest/${name}-${ver}/g" "$src" >"$tmp/rendered.md"
    cmp -s "$tmp/rendered.md" "$KRAM_STDIN" || fail "$name was rendered after the insert"
    cp "$repo/drafts/$name.md" "$tmp/once.md"
    run 200 "" "$name" "$ver" test@example.com
    [[ $RUN_RC -eq 0 ]] || fail "$name republish exited $RUN_RC"
    cmp -s "$tmp/once.md" "$repo/drafts/$name.md" || fail "$name republish rewrote the changelog"
done

after=$(sha256sum "$root"/drafts/draft-*.md)
[[ $before == "$after" ]] || fail "real drafts were modified"

echo "publish.sh next changelog section: ok"
