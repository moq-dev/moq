# [M] The queue host keeps the queue's disk

## Goal

Agent sessions sharing the self-hosted queue host can never fill its disk to
the point where the `moq-queue` runner fails a check, and a runner that does
crash comes back on its own.

## Plan

### Incident and attribution

On 2026-10-30 the host filled twice (about 03:00 and 07:30 PDT). The original
investigation recorded roughly 480 GB of non-CI growth, later freed. Queue
checks ejected [moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985)
and [moq-dev/moq#6999](https://github.com/moq-dev/moq/pull/6999), and at 07:49
the runner aborted and stayed down for 36 minutes.

Read-only investigation on 2026-11-01 confirmed the capacity failure from
`journalctl -u ci-storage-refresh.service --since 2026-10-30`:
at 03:00:50 PDT, CI used 249371398144 bytes against a proposed
249002205184-byte limit. At 08:05:10 it used 145833361408 bytes against a
proposed 119020105728-byte limit. These are failed refresh calculations,
not evidence that those limits were successfully installed.

The live `ci-storage.py` computes the allowance as current project usage
plus physical free space minus 32 GiB. It has no reserved CI floor; a
periodic refresh cannot prevent other writers consuming space between
refreshes. A fixed CI quota alone would still not reserve physical blocks.

The consumer of the original 480 GB remains unknown. The first retained
`agent-storage-gc` journal entry is 2026-10-30 08:21:49 PDT, after both
incidents. That run recovered 8 GiB. Its logs contain global free space and
selected removals, not per-writer allocation history. Neither those logs nor
today's directory sizes establish which process allocated and freed the
incident data. Do not attribute the bursts to worktrees, MBX, or test scratch
without allocation evidence.

### Configuration ownership and live state

The home host is `nixos`, configured in `kixelated/nixos`, not this
repository's `infra/`. Existing work is owned by the maintainer's host PRs:

- [kixelated/nixos#15](https://github.com/kixelated/nixos/pull/15), open at
  `f4e822e4ce7332be253bfc6ce9a0b8e5c60ba4e0`, owns idle-output GC and runner
  restart recovery. GC is deployed and its timer enabled. The real container
  unit, `github-runner-runner.service`, was active with `Restart=no` on
  2026-11-01; the restart commit is still not active there. Do not recreate
  either fix in MoQ.
- [kixelated/nixos#16](https://github.com/kixelated/nixos/pull/16), open at
  `eb007f8bc296b9782f0a479d53c5bd7d10565a20`, owns the newer host landing
  daemons and ephemeral check/fixer containers. The live
  `quest-land-moq.service` is active with `Restart=on-failure`. Storage work
  must cover this gate as well as the retained Actions runner. A deployment
  branch combines host changes; `main` alone does not describe the live host.

Read-only `findmnt`, `xfs_quota ... state/report`, and `xfs_io -r -c stat`
on 2026-11-01 found:

- `/home`, `/tmp`, and `/var/tmp` share the root XFS filesystem with project
  enforcement on. Their directory project IDs are 0 without project
  inheritance. Only projects 0 and 42000 appear in the quota report.
- CI project 42000 has `limitGiB=null`, a 32 GiB reserve, and eight million
  inodes. Its configured trees include `/nix`, native container state, build
  scratch, and the newer gate's caches and staging trees. The sampled
  `/var/cache/quest-land-moq-check` has project 42000 and inheritance enabled.
- Agent storage extends beyond worktree outputs: home caches (MBX, a media
  build cache), rootless container storage, and user-owned `/var/tmp`
  benchmark output. Sizes are under "Measured peaks" below; they are
  allocated-file counts, not incident attribution or unique physical usage
  with reflinks.
- `/var/lib/quest-land` has project ID 0. The gate and its launcher use
  `PrivateTmp=true`; the host has their private directories under both
  `/tmp` and `/var/tmp`. Capping either shared temporary root indiscriminately
  can therefore make the gate hit the agent quota too.

### Measured peaks (2026-11-01, read-only)

Sources: the per-minute `ci-storage-refresh` journal since 2026-10-26
(7687 samples; free space is `limit - current + 32 GiB`), the user
`agent-storage-gc` journal, systemd's per-unit `written to disk` totals for
621 gate check and fixer runs since 2026-10-30, `xfs_quota report -p`, and
`du -x` of the mutable roots. Nothing was changed or built for this.

- Filesystem: 952.4 GiB XFS root, 615.8 GiB used, 336.6 GiB free, 2% of
  inodes used. Project 0 is charged 265.2 GiB and 3.27M inodes; project
  42000 is charged 426.7 GiB and 5.81M of its 8M inodes. The charged sum
  exceeds physical use by 76 GiB, consistent with XFS charging shared
  (reflinked) extents to every owning file. Limits expressed in charged
  bytes are therefore conservative against physical space.
- CI charged usage: 137-247 GiB from 2026-10-26 to 2026-10-29. From
  2026-10-30 19:00 PDT it grew steadily to 842 GiB at 2026-10-31 22:04,
  leaving 103 GiB free, because finished check and fixer instances kept
  their per-instance caches. A one-off root `systemd-run` purge of those
  caches at 22:24 dropped it from 834 to 384 GiB within three minutes.
  Since then it has stayed between 380 and 476 GiB. Completed instance
  directories now measure 0 MiB. That leak is a CI writer and a gate
  cleanup concern; a CI floor must not be sized to absorb it.
- Gate runs since the purge (286 runs): `quest-land-moq-check` wrote a
  p50 7.0, p90 10.1, and maximum 21.4 GiB per run; rust2 9.6, moq-pro 1.2,
  rind 2.0, quest 0.6, and the fixer up to 25.1 GiB. Over the whole window,
  moq-check peaked at 29.1 and moq-pro at 20.1 GiB. At most five runs
  overlapped, and the largest concurrent total was 45.9 GiB across three.
  The active moq-check instance measured 11.6 GiB with `du`.
- CI resident trees: `/nix` 102.1 GiB, `/var/cache/quest-land-caches`
  222.0 GiB (moq 136.9, moq-pro 66.2, rind 18.7), `/var/cache/ci-mbx`
  51.9 GiB, and `/var/lib/nixos-containers` 45.3 GiB.
- Agent trees: `/home/kixelated` 205.5 GiB (caches 91.8, including MBX
  40.7 and a media build cache 39.1; worktrees 57.2; `.local` 27.9),
  `/tmp` 8.1 GiB, and `/var/tmp` 8.6 GiB, about 222 GiB together. The rest
  of project 0, about 43 GiB, is other host state.
- Non-CI incidents: on 2026-10-30, free space fell to 6 GiB while CI was
  charged under 250 GiB, then rose 578 GiB at 08:00 PDT. That non-CI writer
  remains unattributed.

### Storage separation

Implementation stays in the host repository, extending the existing owners'
work above. Reuse its XFS policy and VM tests:

- Give agent-writable trees a separate enforced project with block and inode
  budgets, covering home caches, rootless container storage, and agent
  temporary/benchmark outputs. Put CI private temporary files and daemon
  state in CI-owned trees before tagging the shared temporary roots. Check
  existing files and inheritance, including rename/hard-link boundaries;
  tagging only a directory does not account for its existing descendants.
- Establish an explicit CI allowance and a bounded budget for every other
  writer on the filesystem, including host services and metadata headroom.
  Their worst-case allocations must fit together with the existing host
  reserve. Never derive the promised floor from a free-space sample or
  silently shrink it when agents grow. Refuse an infeasible plan before
  enabling writers; preserve existing data during any migration.
- Separate the writable Nix storage first. Agent `nix develop` and CI use
  the host daemon and `/nix` in project 42000, so a home/tmp cap cannot stop
  agents exhausting that shared allowance. Give agents and CI separate
  writable Nix stores and temporary build trees, each inside its own
  project, rather than tagging only their checkouts. Agents lose today's
  shared-store cache reuse with CI; their store counts against the agent
  budget.
- Add bounded allocation evidence to the host's existing logging: physical
  free space, per-project blocks/inodes, and sizes of the identified mutable
  storage roots. Record timestamps and failures. Avoid recursive whole-disk
  scans every refresh and avoid claiming reflink totals are physical usage.

The [XFS project-quota documentation](https://man7.org/linux/man-pages/man8/xfs_quota.8.html)
describes recursive initialization, inheritance, and project boundaries.
Project limits bound allocations; they do not provision a separate volume.

Decided 2026-11-01 (maintainer, /quest-complete iteration of
[moq-dev/moq#7548](https://github.com/moq-dev/moq/pull/7548)):

1. **Owner:** ✅ extend the existing host owner's work / a separate host PR.
2. **Isolation:** ✅ keep the full Goal, with separate writable Nix storage
   for agents and for CI / narrow the guarantee / a dedicated CI filesystem.
3. **Capacity:** ✅ measure representative gate peaks, then approve the CI
   floor, agent block/inode limits, and residual host budget together. No
   value is selected without that approval.

Capacity proposal, awaiting the maintainer's approval. All four fit the
952.4 GiB filesystem together (952 GiB), in charged bytes:

| Budget | Proposed | Measured basis |
| --- | --- | --- |
| CI floor (project 42000) | 520 GiB, 8M inodes | 476 GiB peak since the leak purge, leaving 44 GiB, near the 45.9 GiB largest concurrent-run write; 5.81M inodes used |
| Agent limit (new project) | 340 GiB, 6M inodes | 222 GiB of agent trees plus at most the 102 GiB of today's shared `/nix` for a separate agent store |
| Residual host budget | 60 GiB | about 43 GiB of other project 0 state, plus uncharged inode and log metadata |
| Existing host reserve | 32 GiB | unchanged |

The historical 300 GiB CI cap is not a safe default: CI has been charged
380-476 GiB since the purge.

Remaining work is attribution of the original growth where evidence permits,
the approved host source change, and verification in the host's XFS VM tests:
agent block/inode exhaustion must leave CI writable; cover private temporary
files, shared-store policy, existing-file migration, concurrent writers,
disabled enforcement, and restart recovery for the services still in use.
Confirm with the gate's owner that finished check and fixer instances now
release their caches, so the CI floor never absorbs that leak again.
Live migration, deployment, cleanup, and quota changes remain separate
maintainer actions. This checkpoint changes none of them.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m0 quest / note in the host configuration only / drop.

Public API: none. Wire: none.
