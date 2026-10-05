#!/bin/sh
# Tests install.sh against fake releases served over file://, with uname,
# getconf, and sysctl faked so every target runs on any host.
#
# Usage: install.test.sh [SHELL...] (default: sh, plus dash if present)
#
# Not busybox: its sh runs its own uname applet, which no fake can replace.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
script="$here/install.sh"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Fake host tools, driven by FAKE_OS, FAKE_ARCH, FAKE_GLIBC, and FAKE_ROSETTA.
mkdir "$tmp/fake"
cat >"$tmp/fake/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) echo "$FAKE_OS" ;;
    -m) echo "$FAKE_ARCH" ;;
esac
EOF
cat >"$tmp/fake/getconf" <<'EOF'
#!/bin/sh
[ -n "$FAKE_GLIBC" ] || exit 1
echo "glibc $FAKE_GLIBC"
EOF
cat >"$tmp/fake/sysctl" <<'EOF'
#!/bin/sh
echo "${FAKE_ROSETTA:-0}"
EOF
chmod +x "$tmp/fake/uname" "$tmp/fake/getconf" "$tmp/fake/sysctl"

# Publish a fake moq-cli release with one tarball per target, laid out as
# release-binary.yml does.
releases="$tmp/releases"
release() {
    version=$1
    tag="moq-cli-v$version"
    mkdir -p "$releases/$tag"
    for target in aarch64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
        root="$tmp/build/$tag-$target"
        mkdir -p "$root/bin"
        printf '#!/bin/sh\necho "moq %s"\n' "$version" >"$root/bin/moq"
        chmod +x "$root/bin/moq"
        tar -czf "$releases/$tag/$tag-$target.tar.gz" -C "$tmp/build" "$tag-$target"
    done
    (
        cd "$releases/$tag"
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum ./*.tar.gz
        else
            shasum -a 256 ./*.tar.gz
        fi | sed 's|  \./|  |' >SHA256SUMS
    )
}
release 1.0.0
release 1.1.0

# The script as moq.sh serves it, with 1.1.0 as the default.
sed 's/@MOQ_VERSION@/1.1.0/' "$script" >"$tmp/served.sh"

failures=0
url=
fail() {
    echo "FAIL [$shell] $*" >&2
    failures=$((failures + 1))
}

# run SCRIPT ARGS...: run under $shell on the faked host, output in $tmp/out.
run() {
    file=$1
    shift
    PATH="$tmp/fake:$PATH" MOQ_RELEASES_URL="${url:-file://$releases}" \
        $shell "$file" "$@" >"$tmp/out" 2>&1
}

# ok NAME SCRIPT ARGS...: the install succeeds.
ok() {
    name=$1
    shift
    run "$@" || fail "$name: exited $?: $(cat "$tmp/out")"
}

# refuse NAME PATTERN SCRIPT ARGS...: the install fails, saying PATTERN.
refuse() {
    name=$1
    pattern=$2
    shift 2
    if run "$@"; then
        fail "$name: succeeded"
    elif ! grep -q "$pattern" "$tmp/out"; then
        fail "$name: want '$pattern', got: $(cat "$tmp/out")"
    fi
}

# installed WANT: the binary in $dir reports WANT.
installed() {
    got=$("$dir/moq" 2>&1 || true)
    [ "$got" = "moq $1" ] || fail "want moq $1 installed, got '$got'"
}

host() {
    FAKE_OS=$1
    FAKE_ARCH=$2
    FAKE_GLIBC=${3:-}
    FAKE_ROSETTA=${4:-0}
    export FAKE_OS FAKE_ARCH FAKE_GLIBC FAKE_ROSETTA
}

suite() {
    dir="$tmp/$(echo "$shell" | tr ' /' '__')/bin"

    host Linux x86_64 2.35
    ok "fresh install" "$tmp/served.sh" --dir "$dir"
    installed 1.1.0
    ok "explicit downgrade" "$tmp/served.sh" --dir "$dir" --version 1.0.0
    installed 1.0.0
    ok "upgrade to the default" "$tmp/served.sh" --dir "$dir"
    installed 1.1.0

    # The documented form: the script arrives on stdin.
    piped="$tmp/piped/bin"
    PATH="$tmp/fake:$PATH" MOQ_RELEASES_URL="file://$releases" \
        $shell -s -- --dir "$piped" <"$tmp/served.sh" >"$tmp/out" 2>&1 ||
        fail "piped install: $(cat "$tmp/out")"
    [ -x "$piped/moq" ] || fail "piped install: no binary"

    # A download cut off anywhere before the last line installs nothing.
    cut="$tmp/cut/bin"
    lines=$(wc -l <"$tmp/served.sh")
    n=1
    while [ "$n" -lt "$lines" ]; do
        head -n "$n" "$tmp/served.sh" |
            PATH="$tmp/fake:$PATH" MOQ_RELEASES_URL="file://$releases" \
                $shell -s -- --dir "$cut" >/dev/null 2>&1 || true
        [ ! -e "$cut/moq" ] || fail "truncated to $n lines: installed anyway"
        n=$((n + 1))
    done

    refuse "unserved script needs a version" "no default version" "$script" --dir "$dir"
    ok "unserved script with a version" "$script" --dir "$dir" --version 1.0.0
    installed 1.0.0

    refuse "malformed version" "invalid version" "$tmp/served.sh" --dir "$dir" --version 1.0
    refuse "missing version" "not found" "$tmp/served.sh" --dir "$dir" --version 9.9.9
    refuse "unknown option" "unknown option" "$tmp/served.sh" --nope
    refuse "missing value" "needs a value" "$tmp/served.sh" --dir
    installed 1.0.0

    # A release whose SHA256SUMS lacks this target is incomplete.
    cp -R "$releases/moq-cli-v1.1.0" "$releases/moq-cli-v1.2.0"
    grep -v aarch64-unknown-linux-gnu "$releases/moq-cli-v1.1.0/SHA256SUMS" >"$releases/moq-cli-v1.2.0/SHA256SUMS"
    host Linux aarch64 2.34
    refuse "incomplete release" "has no moq-cli-v1.2.0-aarch64-unknown-linux-gnu.tar.gz" "$tmp/served.sh" --dir "$dir" --version 1.2.0
    installed 1.0.0

    # A tarball that doesn't match SHA256SUMS is never installed.
    cp -R "$releases/moq-cli-v1.1.0" "$releases/moq-cli-v1.3.0"
    echo corrupt >"$releases/moq-cli-v1.3.0/moq-cli-v1.1.0-x86_64-unknown-linux-gnu.tar.gz"
    sed 's/moq-cli-v1\.1\.0/moq-cli-v1.3.0/' "$releases/moq-cli-v1.1.0/SHA256SUMS" >"$releases/moq-cli-v1.3.0/SHA256SUMS"
    for f in "$releases/moq-cli-v1.3.0"/moq-cli-v1.1.0-*; do
        mv "$f" "$(echo "$f" | sed 's/moq-cli-v1\.1\.0-/moq-cli-v1.3.0-/')"
    done
    host Linux x86_64 2.35
    refuse "corrupt archive" "checksum mismatch" "$tmp/served.sh" --dir "$dir" --version 1.3.0
    installed 1.0.0
    rm -rf "$releases/moq-cli-v1.2.0" "$releases/moq-cli-v1.3.0"

    # A binary that can't run on this host never replaces a working one.
    cp -R "$releases/moq-cli-v1.1.0" "$releases/moq-cli-v1.4.0"
    (
        cd "$releases/moq-cli-v1.4.0"
        name=moq-cli-v1.4.0-x86_64-unknown-linux-gnu
        mkdir -p "$tmp/broken/$name/bin"
        printf '#!/bin/sh\nexit 1\n' >"$tmp/broken/$name/bin/moq"
        chmod +x "$tmp/broken/$name/bin/moq"
        rm -f ./*.tar.gz
        tar -czf "$name.tar.gz" -C "$tmp/broken" "$name"
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$name.tar.gz"
        else
            shasum -a 256 "$name.tar.gz"
        fi >SHA256SUMS
    )
    refuse "binary fails to run" "fails to run" "$tmp/served.sh" --dir "$dir" --version 1.4.0
    installed 1.0.0
    [ -z "$(find "$dir" -name '.moq.*')" ] || fail "staged file left behind"
    rm -rf "$releases/moq-cli-v1.4.0"

    url=http://example.com
    refuse "plain http mirror" "must be https" "$tmp/served.sh" --dir "$dir"
    url=

    host Linux aarch64 2.34
    ok "linux aarch64" "$tmp/served.sh" --dir "$dir"
    installed 1.1.0
    host Darwin arm64
    ok "macOS arm64" "$tmp/served.sh" --dir "$dir" --version 1.0.0
    installed 1.0.0
    host Darwin x86_64 "" 1
    ok "macOS under Rosetta" "$tmp/served.sh" --dir "$dir"
    installed 1.1.0

    host Darwin x86_64
    refuse "intel mac" "only Apple silicon" "$tmp/served.sh" --dir "$dir"
    host Linux riscv64 2.39
    refuse "unsupported arch" "x86_64 and aarch64" "$tmp/served.sh" --dir "$dir"
    host FreeBSD amd64
    refuse "unsupported os" "winget" "$tmp/served.sh" --dir "$dir"
    host Linux x86_64 2.31
    refuse "old glibc" "has 2.31" "$tmp/served.sh" --dir "$dir"
    host Linux x86_64
    refuse "musl" "no glibc" "$tmp/served.sh" --dir "$dir"
    installed 1.1.0

    # A package manager's symlink is never overwritten.
    host Linux x86_64 2.35
    link="$tmp/link-$$"
    mkdir -p "$link"
    ln -sf "$dir/moq" "$link/moq"
    refuse "symlink destination" "is a symlink" "$tmp/served.sh" --dir "$link"
    rm -rf "$link"

    # An unwritable directory fails before touching the old binary.
    if [ "$(id -u)" != 0 ]; then
        chmod 555 "$dir"
        refuse "read-only directory" "cannot write" "$tmp/served.sh" --dir "$dir" --version 1.0.0
        chmod 755 "$dir"
        installed 1.1.0
    fi
}

if [ $# -eq 0 ]; then
    set -- sh
    command -v dash >/dev/null 2>&1 && set -- "$@" dash
fi
for shell in "$@"; do
    suite
done

if [ "$failures" -ne 0 ]; then
    echo "$failures failure(s)" >&2
    exit 1
fi
echo "install.sh: all tests passed under: $*"
