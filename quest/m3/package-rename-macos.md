# [S] `just check` passes on macOS through the package rename test

## Goal

`sh/rs/package-rename.test.py` passes on macOS, or the check selects it only on
hosts that can run it, so `just check` on a Mac reports no failure the branch
did not cause.

## Plan

Found while validating [moq-dev/moq#9825](https://github.com/moq-dev/moq/pull/9825)
on macOS: the test fails identically on `main`. The dev shell's `rpm` aborts on
`rpm --dbpath <scratch> -qp --provides <package>.rpm` with
`libc++abi: terminating due to uncaught exception of type std::__1::system_error: mutex lock failed: Invalid argument`
(SIGABRT), after `--initdb` succeeds.

Recommended: find whether nixpkgs' darwin `rpm` can query a package at all
(another flag, or an env such as a database backend that avoids the lock).
If it cannot, read the package's provides and file list without `rpm`, rather
than skipping the check on macOS, so the test still runs on every host.

## Hosts

- macos - the failure reproduces only there
