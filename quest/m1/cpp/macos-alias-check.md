# [XS] The alias check runs on macOS

## Goal

`just cpp check` passes its "every generated `moq::MoqFoo` has a `moq::Foo`
alias" step on macOS outside the dev shell, so the C++23 probe after it runs.
Today the second `sed -nE` in `cpp/justfile` matches with a backreference
(`= Moq\1;`), which BSD `sed` ignores, so the alias list comes back empty and
every type reports missing. CI runs Ubuntu with GNU `sed` and never sees it.

## Plan

Extract both names from each `using` line and compare them in `awk` (or
another portable tool) instead of a backreference in the pattern. Check it
with `/usr/bin/sed` first on PATH.

Public API: none. Wire: none.
