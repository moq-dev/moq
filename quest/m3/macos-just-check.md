# [S] just check passes on macOS

## Goal

`just check` passes on an aarch64-darwin host at `main`, so a change checked on a Mac reports only its own failures.

## Plan

Found 2026-11-28 on [moq-dev/moq#9574](https://github.com/moq-dev/moq/pull/9574), on the Mac mini (aarch64-darwin), with both failures reproduced unchanged in a checkout of `main`:

- `check rs_cross` (`sh/rs/select.sh cross --all`): the `alloca` 0.4.0 build script, which Criterion pulls in, fails compiling `alloca.c` with `-flto` through mbx's C compiler shim: `error: LTO requires using LLD`. Find whether the cross target's C toolchain or the shim drops the linker setting `alloca` expects, and fix it there.
- `check tooling`: `sh/rs/package-rename.test.py` aborts in `rpm -qp --provides` on the nfpm RPM (`libc++abi: ... mutex lock failed: Invalid argument`, SIGABRT). Find whether nixpkgs' darwin `rpm` is broken for queries, and either use a working build or query the RPM with a tool that runs on darwin.

A fresh `nix develop` on the same host also failed building `swift-collections` 1.7.6 (`install_name_tool`: larger load commands do not fit, needs `-headerpad_max_install_names`), so the dev shell built only where it was already cached. Check whether nixpkgs fixed it before working around it.

Public API: none. Wire: none.

## Hosts

- macos - the failures only reproduce there
