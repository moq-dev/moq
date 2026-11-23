#!/usr/bin/env bash
# Print each path a Rust source under rs/ reads outside its crate, or inside its
# crate's `tests/`, `benches/`, or `examples/`, one
# `<crate dir> <glob> <file>:<line>` per line, naming the literal behind it. A glob's `*`, like a format interpolation such
# as `{name}`, stays within a path segment, `**` crosses them, and a plain path
# also covers what lies beneath it.
#
# A read is an `include_*!` or `#[path]` literal, relative to its file, or, in a
# file that mentions CARGO_MANIFEST_DIR, a literal that climbs out with `../`,
# relative to its crate. A `#[path]` to a `mod.rs` reads its directory, where its
# own modules live. The CARGO_MANIFEST_DIR rule spans the whole file rather than
# the statement that joins the literal, so a read built across statements is
# never missed; the cost is that any `../` string in such a file, such as a
# path-traversal test input, also counts, and the source column points at it. sh/rs/select.sh selects the reading crate from these, and
# sh/rs/select.test.py checks that sh/dispatch.sh's `rs` scope covers those
# outside rs/. The interop clients under test/ are moq-cli examples that only
# the interop harness runs, so their scripts stay out.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

crates=$(git ls-files --cached --others --exclude-standard -- ':(glob)rs/**/Cargo.toml' | sed 's|/Cargo\.toml$||')
# grep exits 1 when no source climbs, which leaves nothing to print.
files=$(grep -rlE --include='*.rs' --exclude-dir=target '"(\{\}/|/)?\.\./' rs) || [[ $? == 1 ]]
[[ -n "$files" ]] || exit 0

# Only a literal that climbs can be a read, so jq sees just each such line and the one
# before it, where a multi-line `include_str!(` opens (rustfmt keeps it there). Scanning
# whole files made jq's regex cost grow with file size times matches.
# Each window opens with a `:<first line> <file>` header and every body line has a
# leading space, so no byte in a file (including NUL) ever collides with the frame
# boundary. Whether a file mentions CARGO_MANIFEST_DIR, which the rule below spans the
# whole file for, comes from its own list. One argument per file; no path under rs/
# holds whitespace.
# shellcheck disable=SC2086
manifests=$(grep -l CARGO_MANIFEST_DIR $files) || [[ $? == 1 ]]
# shellcheck disable=SC2086
awk '
    FNR == 1 { prev = ""; n = 0 }
    { n++ }
    $0 ~ "\"(\\{\\}/|/)?\\.\\./" {
        if (n > 1) { print ":" (n - 1) " " FILENAME; print " " prev }
        else print ":" n " " FILENAME
        print " " $0
    }
    { prev = $0 }
' $files |
    jq -rRs --arg crates "$crates" --arg manifests "$manifests" '
    def resolve: reduce split("/")[] as $s ([];
        if $s == ".." then .[:-1] elif $s == "." or $s == "" then . else . + [$s] end) | join("/");
    ($crates | split("\n") | map(select(. != ""))) as $crates
    | ltrimstr(":")
    | split("\n:")
    | .[]
    | select(. != "")
    | split("\n ")
    | (.[0] | split(" ")) as [$first, $file]
    | (.[1:] | join("\n")) as $text
    | ($file | split("/")[:-1] | join("/")) as $dir
    | ([$crates[] | select(. as $c | $file | startswith($c + "/"))] | max_by(length)) as $crate
    | ($manifests | split("\n") | index($file) != null) as $manifest
    | $text
    | match("(include(?:_str|_bytes)?!\\s*\\(\\s*|#\\[path\\s*=\\s*)?\"((?:\\{\\}/|/)?\\.\\./[^\"]*)\""; "g")
    | "\($file):\(($first | tonumber) + ($text[:.offset] | split("\n") | length) - 1)" as $source
    | .captures as [$from, $name]
    | if $from.string != null then "\($dir)/\($name.string)"
      elif $manifest then "\($crate)/\($name.string | ltrimstr("{}"))"
      else empty end
    | gsub("\\{[^}]*\\}"; "*")
    | resolve
    | if ($from.string // "") | startswith("#") then sub("/mod\\.rs$"; "") else . end
    # A read inside the crate matters only into its target directories, whose
    # changes otherwise reach no dependent: from its library, or from a target
    # that a target of another crate includes in turn.
    | select((startswith($crate + "/") | not) or (ltrimstr($crate + "/") | test("^(tests|benches|examples)/")))
    | "\($crate) \(.) \($source)"
' | LC_ALL=C sort -u
