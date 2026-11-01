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
plus physical free space minus 32 GiB. CI therefore fails once free space
falls under that 32 GiB reserve, whoever consumed it. Nothing bounds the
other writers, so today an agent can take the space CI needs.

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
  `27700296eb7357001a1c44257cb6c140236b5925`, owns the newer host landing
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
  benchmark output. Sizes are under "Measured peaks" below; they are `du`
  allocated block usage, not inode counts, incident attribution, or unique
  physical usage with reflinks.
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
  directories now measure 0 MiB. That leak is CI's own growth, which the
  gate's cleanup must prevent (below); no storage cap absorbs it.
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
work above. Reuse its XFS policy and VM tests. Everything stays on the one
shared XFS filesystem; the only new limit is a cap on agents.

- Give agent-writable trees one enforced project with a 340 GiB block limit
  in charged bytes and a 10M inode limit. It covers home caches, rootless
  container storage, agent temporary/benchmark outputs, and the agent Nix
  store below. Put CI private temporary files and daemon state in CI-owned
  trees before tagging the shared temporary roots. Check existing files and
  inheritance, including rename/hard-link boundaries; tagging only a
  directory does not account for its existing descendants. Preserve
  existing data during any migration.
- Separate the writable Nix storage first. Agent `nix develop` and CI use
  the host daemon and `/nix` in project 42000, so a home/tmp cap cannot stop
  agents exhausting the space CI uses. Give agents and CI separate writable
  Nix stores and temporary build trees, rather than tagging only their
  checkouts. The agent store and its build trees are inside the agent
  project and count against its 340 GiB. Agents lose today's shared-store
  cache reuse with CI.
- CI and the rest of the host share whatever the agent cap leaves, with no
  CI floor and no separate host budget. `ci-storage.py` keeps its existing
  free-space allowance and 32 GiB reserve.
- Add bounded allocation evidence to the host's existing logging: physical
  free space, per-project blocks/inodes, and sizes of the identified mutable
  storage roots. Record timestamps and failures. Avoid recursive whole-disk
  scans every refresh and avoid claiming reflink totals are physical usage.

The [XFS project-quota documentation](https://man7.org/linux/man-pages/man8/xfs_quota.8.html)
describes recursive initialization, inheritance, and project boundaries.
Project limits bound allocations; they do not provision a separate volume.

The agent limit is sized from the measured peaks: 222 GiB of agent trees
plus at most 102 GiB for an agent store sized like today's shared `/nix`,
324 GiB together, leaving agents 16 GiB of headroom. The agent store's real
size can only be measured after the separation deploys; resize the limit
from that measurement. The inode limit is sized the same way. All of
project 0 holds 3.27M inodes today. `/nix`'s own count was not measured,
but it is part of project 42000's 5.81M, so an agent store like it adds at
most 5.81M, about 9.1M together. The 10M limit covers that bound. Before
deployment, count `/nix` inodes read-only, and after the separation resize
the limit from the agent project's measured inode use.

The cap keeps the Goal with a 56 GiB margin. CI fails once free space
falls under its 32 GiB reserve, so writers can use 920.4 GiB before CI
fails. Agents at their cap hold 340 GiB plus uncharged inode metadata,
under 5 GiB at 10M inodes of 512 bytes. That leaves about 575 GiB for CI and
other host state. Their measured peak together is 519 GiB (CI 476, other
host state 43), so 56 GiB remains, more than the 45.9 GiB largest
concurrent gate write. Charged bytes overstate physical use under reflinks,
so the margin is conservative.

The cap covers only agent writes inside the agent project. By design it
does not stop CI or other host state growing past 575 GiB on its own: the
842 GiB cache leak plus 43 GiB of host state would exceed that even with
agents idle. The unattributed 2026-10-30 burst left 6 GiB free with CI
charged under 250 GiB. CI's charged bytes bound its physical use from
above, so the extents non-CI files referenced then covered at least
696 GiB of the 946 GiB used, and their charged bytes were at least that.
If agents wrote it in covered trees, the cap would have stopped them at
340 GiB. If a host
service or an agent path outside the project wrote it, the cap would not.
Attribute it where evidence permits, and after migration verify that no
agent-owned data remains outside the agent project.

The gate's owner, [kixelated/nixos#16](https://github.com/kixelated/nixos/pull/16),
must make every finished check and fixer instance release its caches, so
CI's resident use stays near its 380-476 GiB post-purge range. Its current
head adds no such cleanup, and the 2026-10-31 purge was a one-off. Confirm
the cleanup holds across the gate's run history.

Decided 2026-11-01 (maintainer, /quest-complete iterations of
[moq-dev/moq#7548](https://github.com/moq-dev/moq/pull/7548)):

1. **Owner:** ✅ extend the existing host owner's work / a separate host PR.
2. **Isolation:** ✅ keep the full Goal, with separate writable Nix storage
   for agents and for CI / narrow the guarantee / a dedicated CI filesystem.
3. **Capacity measurement:** ✅ measure representative gate peaks before
   approving any values / pick values without measuring.
4. **Capacity shape:** ✅ cap agents only: one agent quota of about 340 GiB
   plus an inode limit, sized from the measured agent peak and the agent
   Nix store; CI and the rest of the host share what is left, with no CI
   floor or host budget, and gate cleanup handles CI's cache leak / A: CI
   floor 520 GiB, agents 340 GiB, host budget 60 GiB / B: CI floor
   560 GiB, agents 300 GiB, host budget 60 GiB. The maintainer first asked
   why CI and the host could not share the disk; it already is one shared
   filesystem, and the limits are only caps on it. This drops the CI floor
   and host budget that decision 3 had planned to approve.

Remaining work is attribution of the original growth where evidence permits,
the host source change, the gate's cache cleanup, and verification in the
host's XFS VM tests: agent block/inode exhaustion must leave CI writable;
cover private temporary files, the separate Nix stores, existing-file
migration, concurrent writers, disabled enforcement, and restart recovery
for the services still in use. Live migration, deployment, cleanup, and
quota changes remain separate maintainer actions. This checkpoint changes
none of them.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m0 quest / note in the host configuration only / drop.

Public API: none. Wire: none.
