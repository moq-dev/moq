#!/usr/bin/env bash
# publish.sh opens the next changelog section only after the datatracker
# accepts the submission. A Since-<prev> (in progress) heading publishes as
# prev+1. The first submission and a retry both send that heading closed,
# and neither sends a next section that is already open.
# Fixtures stand in for curl and kramdown-rfc.
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
# A template argument is the changelog temp beside the draft. Fail that call
# when FAKE_MKTEMP_FAIL is set. Dropping write bits does not stop root.
real_mktemp=$(command -v mktemp)
cat >"$bin/mktemp" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [[ -n \${FAKE_MKTEMP_FAIL:-} && \$# -gt 0 ]]; then
    echo "fake mktemp failed" >&2
    exit 1
fi
"$real_mktemp" "\$@"
EOF
chmod +x "$bin/curl" "$bin/kramdown-rfc" "$bin/mktemp"

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
            # Since-<prev> (in progress) is the unpublished section for prev+1.
            if [[ ${lines[$i]} =~ ^##\ Since\ .*-([0-9][0-9])\ \(in\ progress\)$ ]]; then
                printf '%02d\n' $((10#${BASH_REMATCH[1]} + 1))
                return 0
            fi
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

# Changelog version headings, in order, until the next top-level section.
changelog_version_headings() {
    local file=$1
    local -a lines=()
    mapfile -t lines <"$file"
    local -i i n=${#lines[@]} start=-1
    for ((i = 0; i < n; i++)); do
        if [[ ${lines[$i]} =~ ^#\ .*Changelog[[:space:]]*$ ]]; then
            start=$i
            break
        fi
    done
    ((start >= 0)) || return 1
    for ((i = start + 1; i < n; i++)); do
        if [[ ${lines[$i]} =~ ^#[[:space:]] ]]; then
            break
        fi
        if [[ ${lines[$i]} =~ ^##\ .*-([0-9][0-9])(\ \(in\ progress\))?$ ]]; then
            printf '%s\n' "${lines[$i]}"
        fi
    done
}

assert_opened() {
    local before_file=$1
    local after_file=$2
    local ver=$3
    local next=$4
    local -a before_lines after_lines
    mapfile -t before_lines <"$before_file"
    mapfile -t after_lines <"$after_file"
    local -i changelog=-1 i n=${#before_lines[@]}
    for ((i = 0; i < n; i++)); do
        if [[ ${before_lines[$i]} =~ ^#\ .*Changelog[[:space:]]*$ ]]; then
            changelog=$i
            break
        fi
    done
    ((changelog >= 0)) || fail "$before_file has no changelog"
    local -i old_idx=-1
    for ((i = changelog + 1; i < n; i++)); do
        if [[ ${before_lines[$i]} =~ ^#[[:space:]] ]]; then
            break
        fi
        if [[ ${before_lines[$i]} =~ ^##\ .*-([0-9][0-9])(\ \(in\ progress\))?$ ]]; then
            old_idx=$i
            break
        fi
    done
    ((old_idx >= 0)) || fail "$before_file changelog has no version heading"
    local old=${before_lines[$old_idx]}
    local closed=$old
    local new
    if [[ $old =~ ^##\ Since\ (.+)-([0-9][0-9])\ \(in\ progress\)$ ]]; then
        local stem=${BASH_REMATCH[1]}
        local prev=${BASH_REMATCH[2]}
        local expect_ver
        expect_ver=$(printf '%02d' $((10#$prev + 1)))
        [[ $ver == "$expect_ver" ]] || fail "since heading $old publishes as $expect_ver, not $ver"
        closed="## Since ${stem}-${prev}"
        new="## Since ${stem}-${ver} (in progress)"
    else
        [[ $old =~ ^##\ .*-${ver}(\ \(in\ progress\))?$ ]] || fail "published heading $old is not version $ver"
        new=${old/$ver/$next}
    fi
    local -a attrs=()
    local -i j=$((old_idx + 1))
    while ((j < n)) && [[ ${before_lines[$j]} =~ ^\{: ]]; do
        attrs+=("${before_lines[$j]}")
        j+=1
    done
    local -a expect_lines=()
    local -i b
    for ((b = 0; b < old_idx; b++)); do
        expect_lines+=("${before_lines[$b]}")
    done
    expect_lines+=("$new")
    if ((${#attrs[@]} > 0)); then
        local attr
        for attr in "${attrs[@]}"; do
            expect_lines+=("$attr")
        done
    fi
    expect_lines+=("")
    expect_lines+=("$closed")
    for ((b = old_idx + 1; b < n; b++)); do
        expect_lines+=("${before_lines[$b]}")
    done
    local mismatch=0
    if ((${#expect_lines[@]} != ${#after_lines[@]})); then
        mismatch=1
    else
        for ((b = 0; b < ${#expect_lines[@]}; b++)); do
            if [[ ${expect_lines[$b]} != "${after_lines[$b]}" ]]; then
                mismatch=1
                break
            fi
        done
    fi
    if ((mismatch != 0)); then
        printf '%s\n' "${expect_lines[@]}" >"$tmp/expect-opened.md"
        diff -u "$tmp/expect-opened.md" "$after_file" >&2 || true
        fail "$before_file did not gain an empty next section"
    fi
}

# Submitted markdown is the source. A Since `(in progress)` heading is closed.
# The next section, when the file already has one, is omitted by the publisher.
submitted_source() {
    local src=$1
    local out=$2
    if grep -q '^## Since ' "$src"; then
        sed 's/^\(## Since .*\) (in progress)$/\1/' "$src" >"$out"
    else
        cp "$src" "$out"
    fi
}

# A retry still has the next heading in the file, and the text sent to
# kramdown does not. Versioned submissions match the pre-insert source.
# Since submissions match it with ` (in progress)` removed from that heading.
assert_retry_submission() {
    local name=$1
    local src=$2
    local version=$3
    local file=$repo/drafts/$name.md
    local -a headings=()
    mapfile -t headings < <(changelog_version_headings "$file")
    ((${#headings[@]} >= 2)) || fail "$name retry has no next section"
    local next_heading=${headings[0]}
    local published_heading=${headings[1]}
    grep -qxF "$next_heading" "$file" || fail "$name file lost the next heading"
    if grep -qxF "$next_heading" "$KRAM_STDIN"; then
        fail "$name retry submitted the next heading: $next_heading"
    fi
    grep -qxF "$published_heading" "$KRAM_STDIN" || fail "$name retry dropped $published_heading"
    submitted_source "$src" "$tmp/retry-src.md"
    sed "s/${name}-latest/${name}-${version}/g" "$tmp/retry-src.md" >"$tmp/retry-want.md"
    cmp -s "$tmp/retry-want.md" "$KRAM_STDIN" || {
        diff -u "$tmp/retry-want.md" "$KRAM_STDIN" >&2 || true
        fail "$name retry submitted a next section or rewrote entries"
    }
}

expect_file() {
    local name=$1
    local src=$2
    local want=$3
    cmp -s "$want" "$repo/drafts/$name.md" || {
        diff -u "$want" "$repo/drafts/$name.md" >&2 || true
        fail "$name did not match the expected changelog"
    }
    # The first submission closes a Since heading and does not include a next
    # section. It matches a later retry of the same source.
    submitted_source "$src" "$tmp/render-src.md"
    cmp -s "$tmp/render-src.md" "$KRAM_STDIN" || {
        diff -u "$tmp/render-src.md" "$KRAM_STDIN" >&2 || true
        fail "$name submission did not match the closed source"
    }
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

## Since draft-lcurley-moq-hidden-00

- Apply hidden filtering only to opted-in peers.
EOF

cat >"$tmp/since-attr.md" <<'EOF'
# Changelog
{:numbered="false"}

## Since draft-lcurley-moq-solicit-00 (in progress)
{:numbered="false"}

- Declare what you want.
EOF
cat >"$tmp/since-attr-next.md" <<'EOF'
# Changelog
{:numbered="false"}

## Since draft-lcurley-moq-solicit-01 (in progress)
{:numbered="false"}

## Since draft-lcurley-moq-solicit-00
{:numbered="false"}

- Declare what you want.
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
    local mode
    mode=$(stat -c %a "$repo/drafts/$name.md")
    [[ $mode == "$(stat -c %a "$src")" ]] || fail "$label changed the file mode to $mode"
    local once=$tmp/once.md
    cp "$repo/drafts/$name.md" "$once"
    run 201 "" "$name" "$version" test@example.com
    [[ $RUN_RC -eq 0 ]] || fail "$label republish exited $RUN_RC"
    cmp -s "$once" "$repo/drafts/$name.md" || fail "$label republish rewrote the changelog"
    assert_retry_submission "$name" "$src" "$version"
}

check_case "hang" "$tmp/hang.md" draft-lcurley-moq-hang 04 "$tmp/hang-next.md"
check_case "lite" "$tmp/lite.md" draft-lcurley-moq-lite 07 "$tmp/lite-next.md"
check_case "cluster" "$tmp/cluster.md" draft-lcurley-moq-cluster 02 "$tmp/cluster-next.md"
check_case "hidden" "$tmp/hidden.md" draft-lcurley-moq-hidden 01 "$tmp/hidden-next.md"
check_case "since attr" "$tmp/since-attr.md" draft-lcurley-moq-solicit 01 "$tmp/since-attr-next.md"
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

cat >"$tmp/above-xml.md" <<'EOF'
# Changelog

## moq-hang-04

- Published.
EOF
install_draft draft-lcurley-moq-hang "$tmp/above.md"
run 200 "" draft-lcurley-moq-hang 04 test@example.com
[[ $RUN_RC -eq 0 ]] || fail "existing next section exited $RUN_RC"
unchanged "$tmp/above.md" draft-lcurley-moq-hang
[[ -s $CURL_LOG ]] || fail "existing next section skipped submission"
cmp -s "$tmp/above-xml.md" "$KRAM_STDIN" || {
    diff -u "$tmp/above-xml.md" "$KRAM_STDIN" >&2 || true
    fail "existing next section was submitted"
}
grep -qxF '## moq-hang-05' "$repo/drafts/draft-lcurley-moq-hang.md" || fail "existing next heading was removed"

install_draft draft-lcurley-moq-hang "$tmp/hang.md"
run 200 "" draft-lcurley-moq-hang 99 test@example.com
[[ $RUN_RC -ne 0 ]] || fail "version 99 succeeded"
no_submit
unchanged "$tmp/hang.md" draft-lcurley-moq-hang

install_draft draft-lcurley-moq-hang "$tmp/hang.md"
export FAKE_MKTEMP_FAIL=1
run 200 "" draft-lcurley-moq-hang 04 test@example.com
unset FAKE_MKTEMP_FAIL
[[ $RUN_RC -ne 0 ]] || fail "write failure succeeded"
unchanged "$tmp/hang.md" draft-lcurley-moq-hang
grep -q "not updated" "$tmp/err" || fail "write failure did not report the missed update"
if grep -q "Submitted" "$tmp/out"; then
    fail "write failure reported success"
fi
[[ -s $CURL_LOG ]] || fail "write failure skipped submission"
if find "$repo/drafts" -name '*.tmp.*' -print -quit | grep -q .; then
    fail "write failure left a temp file behind"
fi

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
    submitted_source "$src" "$tmp/render-src.md"
    sed "s/${name}-latest/${name}-${ver}/g" "$tmp/render-src.md" >"$tmp/rendered.md"
    cmp -s "$tmp/rendered.md" "$KRAM_STDIN" || fail "$name was rendered after the insert"
    cp "$repo/drafts/$name.md" "$tmp/once.md"
    run 200 "" "$name" "$ver" test@example.com
    [[ $RUN_RC -eq 0 ]] || fail "$name republish exited $RUN_RC"
    cmp -s "$tmp/once.md" "$repo/drafts/$name.md" || fail "$name republish rewrote the changelog"
    assert_retry_submission "$name" "$src" "$ver"
done

after=$(sha256sum "$root"/drafts/draft-*.md)
[[ $before == "$after" ]] || fail "real drafts were modified"

echo "publish.sh next changelog section: ok"
