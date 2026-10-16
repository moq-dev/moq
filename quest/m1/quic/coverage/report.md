# [S] Report moq-quic coverage

## Goal

`just rs coverage <crate>` prints per-file line and region coverage and
writes lcov and HTML, and a nightly job uploads the report for `moq-quic`
as an artifact. There is no threshold and no gate; the report is how later
quests find untested paths.

## Plan

Decided 2026-10-16. `cargo-llvm-cov` and LLVM's `llvm-cov` and
`llvm-profdata` join the Nix dev shell. The LLVM major version must match
rustc's (LLVM 22 for rustc 1.98); the survey ran
`nix shell nixpkgs#cargo-llvm-cov nixpkgs#llvmPackages_22.llvm` with
`LLVM_COV` and `LLVM_PROFDATA` pointed at those tools. Derive the version
from the toolchain rather than hardcoding it, if the flake allows.
