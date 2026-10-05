#!/bin/sh
# Install or upgrade moq from its GitHub release, served at https://moq.sh:
#
#   curl -fsSL https://moq.sh | sh
#   curl -fsSL https://moq.sh | sh -s -- --version 0.14.0 --dir ~/bin
#
# POSIX sh, so `| sh` works under dash and busybox. Everything runs from the
# last line, so a download cut off mid-pipe runs nothing.
set -eu

# moq.sh replaces this with the newest moq-cli release when it deploys.
DEFAULT_VERSION="@MOQ_VERSION@"

# Overridable for tests and mirrors. Plain HTTP would let anyone on the path
# replace both SHA256SUMS and the archive.
RELEASES_URL="${MOQ_RELEASES_URL:-https://github.com/moq-dev/moq/releases/download}"

usage() {
    cat <<EOF
Install or upgrade moq, the MoQ media CLI.

Usage: curl -fsSL https://moq.sh | sh -s -- [options]

Options:
  --version <x.y.z>  Install this moq-cli release (default: $DEFAULT_VERSION)
  --dir <path>       Install into this directory (default: ~/.local/bin)
  -h, --help         Show this help
EOF
}

say() {
    echo "moq.sh: $*" >&2
}

die() {
    say "error: $*"
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

valid_version() {
    printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'
}

# Sets $target to the release target for this host, or refuses it.
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Darwin)
            # A shell under Rosetta reports x86_64 on Apple silicon.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                arch=arm64
            fi
            case "$arch" in
                arm64 | aarch64) target=aarch64-apple-darwin ;;
                *) die "no moq build for macOS on $arch; only Apple silicon is published. Try: cargo install moq-cli" ;;
            esac
            ;;
        Linux)
            case "$arch" in
                x86_64 | amd64) arch=x86_64 ;;
                aarch64 | arm64) arch=aarch64 ;;
                *) die "no moq build for Linux on $arch; x86_64 and aarch64 are published. Try: cargo install moq-cli" ;;
            esac
            check_glibc
            target=$arch-unknown-linux-gnu
            ;;
        *) die "no moq build for $os; macOS and Linux are published. On Windows: winget install moq-dev.moq" ;;
    esac
}

# The Linux builds link glibc 2.34; musl and older glibc can't run them.
check_glibc() {
    glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null || true)
    case "$glibc" in
        "glibc "*) glibc=${glibc#glibc } ;;
        *) die "moq needs glibc 2.34 or newer, and this system has no glibc (musl?). Try: cargo install moq-cli, or docker run moqdev/moq" ;;
    esac
    major=${glibc%%.*}
    minor=${glibc#*.}
    minor=${minor%%.*}
    case "$major.$minor" in
        *[!0-9.]* | .* | *.) die "cannot parse glibc version '$glibc'" ;;
    esac
    if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 34 ]; }; then
        die "moq needs glibc 2.34 or newer; this system has $glibc. Try: cargo install moq-cli, or docker run moqdev/moq"
    fi
}

main() {
    version=$DEFAULT_VERSION
    explicit=
    dir=
    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || die "--version needs a value"
                version=$2
                explicit=1
                shift 2
                ;;
            --dir)
                [ $# -ge 2 ] || die "--dir needs a value"
                dir=$2
                shift 2
                ;;
            -h | --help)
                usage
                return
                ;;
            *) die "unknown option '$1'; see --help" ;;
        esac
    done

    if [ -z "$dir" ]; then
        [ -n "${HOME:-}" ] || die "HOME is not set; pass --dir"
        dir="$HOME/.local/bin"
    fi

    if ! valid_version "$version"; then
        [ -n "$explicit" ] || die "this copy of the installer has no default version; pass --version x.y.z"
        die "invalid version '$version'; expected x.y.z, such as 0.14.0"
    fi

    case "$RELEASES_URL" in
        https://* | file://*) ;;
        *) die "MOQ_RELEASES_URL must be https:// or file://, not '$RELEASES_URL'" ;;
    esac

    detect_target
    need curl
    need tar
    if command -v sha256sum >/dev/null 2>&1; then
        sha256="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        sha256="shasum -a 256"
    else
        die "sha256sum or shasum is required to verify the download"
    fi

    mkdir -p "$dir" || die "cannot create $dir"
    dir=$(
        unset CDPATH
        cd "$dir" && pwd
    )
    dest="$dir/moq"
    if [ -L "$dest" ]; then
        die "$dest is a symlink, probably from a package manager; upgrade moq there or pass --dir"
    fi
    if [ -e "$dest" ] && [ ! -f "$dest" ]; then
        die "$dest exists and is not a file"
    fi

    tmp=
    staged=
    trap 'rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    tmp=$(mktemp -d)

    tag="moq-cli-v$version"
    asset="$tag-$target.tar.gz"
    base="$RELEASES_URL/$tag"

    say "downloading moq $version for $target"
    curl -fsSL --proto-redir =https "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" ||
        die "moq-cli $version not found; see https://github.com/moq-dev/moq/releases"
    expected=$(awk -v f="$asset" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$expected" ] || die "release $tag has no $asset"
    curl -fsSL --proto-redir =https "$base/$asset" -o "$tmp/$asset" || die "cannot download $base/$asset"
    actual=$($sha256 "$tmp/$asset" | awk '{ print $1 }')
    [ "$actual" = "$expected" ] || die "checksum mismatch for $asset; refusing to install it"

    tar -xzf "$tmp/$asset" -C "$tmp" || die "cannot extract $asset"
    bin="$tmp/$tag-$target/bin/moq"
    [ -f "$bin" ] || die "$asset has no bin/moq"

    # Stage beside the destination so the rename is atomic, and run it first:
    # any failure leaves the previous binary untouched.
    staged=$(mktemp "$dir/.moq.XXXXXX" 2>/dev/null) || die "cannot write to $dir"
    cp "$bin" "$staged" || die "cannot write to $dir"
    chmod 755 "$staged"
    installed=$("$staged" --version) || die "moq $version fails to run on this host; keeping the existing install"
    mv -f "$staged" "$dest" || die "cannot replace $dest"
    staged=
    say "installed $installed to $dest"

    found=$(command -v moq || true)
    if [ "$found" != "$dest" ]; then
        case ":$PATH:" in
            *":$dir:"*) say "warning: $found comes first on PATH and runs instead" ;;
            *) say "add $dir to PATH, for example: export PATH=\"$dir:\$PATH\"" ;;
        esac
    fi
}

main "$@"
