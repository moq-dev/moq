# [S] The package-rename tooling test passes on macOS

## Goal

`python3 sh/rs/package-rename.test.py` passes on a macOS host, so a local `just check`
whose diff selects the tooling tests (a workflow edit is enough) gives a clean verdict
there instead of failing on a check the change never touched.

Today it fails on every run on an Apple Silicon Mac: the dev shell's `rpm -qp --provides`
on the built `moq-nfpm.rpm` aborts with `libc++abi: terminating due to uncaught exception
of type std::__1::system_error: mutex lock failed: Invalid argument` (SIGABRT), right after
`rpm --initdb` on a scratch database. Seen on
[moq-dev/moq#9840](https://github.com/moq-dev/moq/pull/9840), whose diff reads none of
the files the test does.

## Plan

Find whether the abort is the Nix `rpm` build on Darwin or how the test drives it (its
scratch `--dbpath` under `$TMPDIR`), and fix that cause. If `rpm` cannot run on Darwin,
decide the test's support at setup from the host and skip loudly with the reason, rather
than failing, since packaging runs on Linux.
