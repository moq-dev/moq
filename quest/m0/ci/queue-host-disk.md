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
- Agent storage extends beyond worktree outputs: directory scans found
  approximately 41 GiB in the user's MBX cache, 39 GiB in a media build
  cache, and 64 GiB in rootless container storage. `/var/tmp` included an
  8.6 GiB user-owned benchmark directory. These are current allocated-file
  counts, not incident attribution or unique physical usage with reflinks.
- `/var/lib/quest-land` has project ID 0. The gate and its launcher use
  `PrivateTmp=true`; the host has their private directories under both
  `/tmp` and `/var/tmp`. Capping either shared temporary root indiscriminately
  can therefore make the gate hit the agent quota too.

### Proposed storage separation, pending maintainer decisions

Keep implementation in the host repository, coordinated with the owners of
the two PRs above. Reuse its XFS policy and VM tests. Recommended direction:

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
- Resolve the shared Nix writer first. Agent `nix develop` and CI use the
  host daemon and `/nix` in project 42000. A home/tmp cap cannot stop agents
  exhausting that shared allowance. Preserving the Goal's full isolation
  requires separating agent and CI writable Nix storage and temporary builds,
  not merely tagging their checkouts. Existing shared-store cache reuse is a
  deliberate host design; changing it needs the maintainer's decision.
- Add bounded allocation evidence to the host's existing logging: physical
  free space, per-project blocks/inodes, and sizes of the identified mutable
  storage roots. Record timestamps and failures. Avoid recursive whole-disk
  scans every refresh and avoid claiming reflink totals are physical usage.

The [XFS project-quota documentation](https://man7.org/linux/man-pages/man8/xfs_quota.8.html)
describes recursive initialization, inheritance, and project boundaries.
Project limits bound allocations; they do not provision a separate volume.

Decisions to return to the foreground before implementation:

1. **Owner:** extend the existing host-owner work (recommended), or explicitly
   assign a separate host PR based on the current gate configuration. Neither
   host PR has been changed by this investigation.
2. **Isolation:** retain the full Goal and separate writable Nix storage
   (recommended), or explicitly narrow the guarantee to direct agent writes
   outside the shared Nix pool. A dedicated CI filesystem is an alternative
   with a clearer capacity boundary, but needs storage/migration approval.
3. **Capacity:** approve the CI floor, agent block/inode caps, and residual host
   budget together after measuring representative gate peaks. No values are
   chosen. The historical 300 GiB CI cap is not a safe default: the current
   quota snapshot already accounts for about 435 GiB in project 42000.

Remaining work is attribution of the original growth where evidence permits,
the approved host source change, and verification in the host's XFS VM tests:
agent block/inode exhaustion must leave CI writable; cover private temporary
files, shared-store policy, existing-file migration, concurrent writers,
disabled enforcement, and restart recovery for the services still in use.
Live migration, deployment, cleanup, and quota changes remain separate
maintainer actions. This checkpoint changes none of them.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m0 quest / note in the host configuration only / drop.

Public API: none. Wire: none.
