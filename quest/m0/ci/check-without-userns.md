# [S] just check finishes on a host without user namespaces

## Goal

On a host that refuses unprivileged user namespaces, `just check` runs every
other module to completion and reports the flake module as not run, with the
reason, instead of failing it and cancelling the lanes behind it. The merge
queue still runs the flake check in full.

## Plan

Seen 2026-10-24/25 in every quest-spawn agent: `check:flake` (`sh/dispatch.sh`)
runs `nix flake check`, whose github-runner VM test (`nix/runner/test.nix`)
fails with `unshare ... uid_map: Operation not permitted`. That failure cancels
other lanes (kt, js, rs tests), so agents fell back to partial checks and
reported them as `just check`.

- Probe for user namespaces once, up front, and report the flake module as
  skipped with that reason; never as passed.
- Alternatively split the VM test out of the default flake check and run it
  only where it can; pick whichever keeps `just ci` in the queue unchanged.
- One module failing must not cancel unrelated lanes either way.

Public API: none. Wire: none.
