# [S] mbx's free-space GC never removes a live target

## Goal

On agent hosts, a cargo call through the mbx shim never deletes a Cargo
target another session is still using. A run that loses disk headroom gets
a clear failure instead of silently evicting the shared cache. The host
settings that keep it so live in `kixelated/dotfiles`.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7742](https://github.com/moq-dev/moq/pull/7742)): milestone m0,
in the CI epic, beside [shared target](/quest/m0/ci/shared-target.md), which
only works around the same GC for long interop runs.

- Incident
  ([comment](https://github.com/moq-dev/moq/pull/7742#issuecomment-6605544835)):
  `mbx[gc]: 11.2 GiB free … under the 50.0 GiB minimum`, then "removed 7
  target directories (… 6 abandoned and 1 live)". It also evicted about
  30 GiB of shared cache objects below `gc.max_size`, because the disk was
  under `gc.min_free_size`.
- This is documented mbx behaviour (`mbx gc --help`): below
  `gc.min_free_size`, it collects past its budgets, managed targets first.
  `~/work/dotfiles/mbx/config.toml` sets no `[gc]` options, so the
  defaults apply. On 2026-11-03, `/` on `desktop` was 98% full with 49 GB
  free, still under the threshold, so any cargo call can trigger it again.
- Work out why the disk is that full first: other worktrees' targets, `.tmp/`
  scratch, Nix store. `quest worktree` reclaims finished checkouts.
- Then set `[gc]` in dotfiles so the free-space trigger never collects a
  live target, for example by lowering `min_free_size` to a real emergency
  floor. If mbx can't spare live targets at all, report that upstream and
  record the issue link here.
- Edit dotfiles in place (`~/work/dotfiles` is live config), and link its
  commit here when done.

Public API: none. Wire: none.

## Related

- [Long test runs keep their built binaries](/quest/m0/ci/shared-target.md) - the interop-side workaround for the same GC
- [The queue host keeps the queue's disk](/quest/m0/ci/queue-host-disk.md) - disk policy for the queue host, configured in kixelated/nixos
