#!/usr/bin/env bash
# Print each path outside its crate that a Rust source under rs/ reads, one
# `<crate dir> <glob>` per line. A glob's `*`, like a format interpolation such
# as `{name}`, stays within a path segment, `**` crosses them, and a plain path
# also covers what lies beneath it.
#
# A read is an `include_*!` or `#[path]` literal, relative to its file, or, in a
# file that mentions CARGO_MANIFEST_DIR, a literal that climbs out with `../`,
# relative to its crate. A `#[path]` to a `mod.rs` reads its directory, where its
# own modules live. sh/rs/select.sh selects the reading crate from these, and
# sh/rs/select.test.py checks that sh/dispatch.sh's `rs` scope covers those
# outside rs/. The interop clients under test/ are moq-cli examples that only
# the interop harness runs, so their scripts stay out.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

crates=$(git ls-files --cached --others --exclude-standard -- ':(glob)rs/**/Cargo.toml' | sed 's|/Cargo\.toml$||')
# grep exits 1 when no source climbs, which leaves nothing to print.
files=$(grep -rlE --include='*.rs' --exclude-dir=target '"(\{\}/|/)?\.\./' rs) || [[ $? == 1 ]]
[[ -n "$files" ]] || exit 0

# One argument per file; no path under rs/ holds whitespace.
# shellcheck disable=SC2086
jq -nrR --arg crates "$crates" '
    def resolve: reduce split("/")[] as $s ([];
        if $s == ".." then .[:-1] elif $s == "." or $s == "" then . else . + [$s] end) | join("/");
    ($crates | split("\n") | map(select(. != ""))) as $crates
    | [inputs | {file: input_filename, line: .}]
    | group_by(.file)[]
    | .[0].file as $file
    | (map(.line) | join("\n")) as $text
    | ($file | split("/")[:-1] | join("/")) as $dir
    | ([$crates[] | select(. as $c | $file | startswith($c + "/"))] | max_by(length)) as $crate
    | ($text | test("CARGO_MANIFEST_DIR")) as $manifest
    | $text
    | match("(include(?:_str|_bytes)?!\\s*\\(\\s*|#\\[path\\s*=\\s*)?\"((?:\\{\\}/|/)?\\.\\./[^\"]*)\""; "g")
    | .captures as [$from, $name]
    | if $from.string != null then "\($dir)/\($name.string)"
      elif $manifest then "\($crate)/\($name.string | ltrimstr("{}"))"
      else empty end
    | gsub("\\{[^}]*\\}"; "*")
    | resolve
    | if ($from.string // "") | startswith("#") then sub("/mod\\.rs$"; "") else . end
    | select(startswith($crate + "/") | not)
    | "\($crate) \(.)"
' $files | LC_ALL=C sort -u
