# [S] just check finishes on a host without user namespaces

## Goal

On a host that refuses unprivileged user namespaces, `just check` runs every
other module to completion and reports the flake module as not run, with the
reason, instead of failing it and cancelling the lanes behind it. The merge
queue still runs the flake check in full.

## Plan

Implemented in [moq-dev/moq#6823](https://github.com/moq-dev/moq/pull/6823).
The dispatcher finishes independent lanes and modules after failures while
retaining a failing exit status. Local Linux checks use a fresh, local-only
Nix derivation to probe repart's `unshare --map-root-user` invocation; only
its explicit namespace-refusal marker skips the flake module. Strict CI
still runs the complete `nix flake check` without a probe.

The same binary succeeds in the host shell but fails to write `uid_map`
inside the Nix builder, so a shell-only probe would misclassify this host.
The 24 dispatcher tests, ShellCheck, shfmt, and nixfmt checks passed.

Remaining: complete `just check` and `just ci`, address findings, then remove
this quest and its links. Both full runs were stopped while the initial
Python, Rust, and C++ lanes still waited for host admission; neither full
gate passed. Resume those checks when capacity is available.

Public API: none. Wire: none.
