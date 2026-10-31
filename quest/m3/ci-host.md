# [M] Gate on a rented host

## Goal

The merge gates for moq-dev/moq, moq-dev/quest, and moq-dev/rust2 run on
one rented always-on host with a warm on-disk cache, instead of the
maintainer's home `nixos` box. Gate entries are no slower than the home
runner's measured times, and the home box is off the merge path. moq.pro's
gate is out of scope: it stays home for security reasons.

## Plan

Decided 2026-10-26 (maintainer):

- Motivation: uptime (home power, network, reboots), freeing the home
  machine, and keeping PR code off the home network.
- Scope: the public repositories only. moq.pro keeps `container@moq-pro` at
  home; this quest does not touch it.
- Each repository keeps its own runner process (kixelated is a user account,
  so its runners register per repository). All three run concurrently in one
  shared systemd-nspawn container, under one user and one mbx store, and mbx's
  machine-wide budget splits the CPU. No lock serializes them. Rejected: a
  container per repository (three cold caches) and moving the repositories
  into moq-dev for one org runner.
- The cache stays on the host's disk: mbx's local store and the host Nix
  store, substituting from `nix.moq.dev`. The host writes to neither R2 nor
  `nix.moq.dev`.
- The host's NixOS config lives in this repository under `infra/`,
  installed with nixos-anywhere. Runner registration tokens are placed by
  hand, never committed.
- Once the rented host passes a day of gate runs, the home `moq-queue`
  runner is deregistered. Rollback is re-registering it.
- `nix/runner` (the persistent-VM module, rejected 2026-10-25) is deleted,
  with its `runner-vm` recipes and flake checks. It can land ahead as its own
  PR. Then update `quest/m2/gpu-ci.md`, which assumes the VM module.
- Independent of `quest land`: the host only changes which machine answers
  `vars.MOQ_QUEUE_RUNNER`.
- If entries are slower than home, step up the host (more cores, or an OVH
  RISE-M), not the VPS tier.

Measured 2026-10-26 on the home host. `container@moq-ci` is capped at
16 threads and 18 GiB.

- Over 42 hours, the queue runner was busy 46% (about 330 runner-hours a
  month), and more than half the time in 20 of 27 active hours. A Check job
  takes a median 3.9 minutes, p90 14.3.
- One hour sampled every 2 s, 86% busy. Cores in use while busy: median 2,
  mean 4.0, p90 9.4, p95 15.3. Only 16% of busy time is above 8 cores, and
  capping at 8 would add about 7% to job time. So the critical path, and
  therefore single-thread speed, sets job time.
- Anonymous memory peaked at 9.4 GiB (p95 3.9), with up to 15 GiB of page
  cache. The container uses about 15 GB of disk.

Hosts compared on 2026-10-26 (provider pages; Geekbench 6 from
vpsbenchmarks). The home 5900XT scores about 2285 single-thread.

| Host | Cores / RAM / disk | Price/mo | Verdict |
|---|---|---|---|
| netcup RS 4000 G12.5 | 12 dedicated EPYC 9645 (GB6 2141 / 13016), 32 GB, 512 GB | €77.83 (12-month) | Recommended: about home speed, within the ~$80 budget |
| OVH VPS-4 2027 | 8 Haswell-class vCores (GB6 1021 / 5294, stability grade D), 24 GB, 200 GB | ~$27 | Rejected: about 2x slower jobs would saturate the runner; VPS-5/6 add cores, not per-core speed |
| OVH RISE-M | Ryzen 9 9900X 12C/24T, 64 GB, 2x512 GB | €99.99 | Fallback, about 1.34x home single-thread |
| OVH RISE-L, Hetzner AX102 | 16C/32T Zen 4/5, 128 GB | ~€150-160 | Most headroom, over budget |
| Dedicated cloud VMs (Hetzner CCX, OVH c3, AWS, GCP) | 16 vCPU | €250+ | Not competitive always-on |
| Per-minute SaaS (Ubicloud, Namespace, Blacksmith, Depot, GitHub larger runners) | ~16 vCPU | ~$200-950 at 330 h | Break-even with a ~$90 box is about 13% duty; revisit only if `quest land` batching cuts runner-hours several-fold |

Remaining work, once [the host is rented](/quest/m3/ci-host-rented.md):

- `infra/` host config: disko, the shared container, three runner services
  with their own work directories and labels, and a deploy recipe.
- Size `NEXTEST_TEST_THREADS` and the container's CPU and memory to the host
  rather than the home box's 24 threads.
- Make mbx collection safe for concurrent jobs before the runners share a
  store. Each moq job runs `mbx gc` before it builds
  (`.github/actions/rust-cache`), and under disk pressure that sweep can evict
  generated source trees another job's next build reads. Verify with
  overlapping jobs under cache pressure. Safe collection need not serialize
  jobs.
- Give moq-dev/quest and moq-dev/rust2 a gate that runs on the rented
  runner. Their Check workflows hardcode hosted runners (`ubuntu-latest`,
  `ubuntu-24.04-arm`), and quest's has no `merge_group` trigger, so a runner
  variable alone leaves them off the host. Route only trusted gate runs
  (queue or `quest land`) to it, never PR pushes. That is a PR in each of
  those repositories.
- Register the runners, point moq's `vars.MOQ_QUEUE_RUNNER` at its runner,
  and compare a day of entry times against the numbers above.
- Retire the home `moq-queue` runner and its container definition.
- Update `infra/README.md` and the CI epic's runner decision.

Public API: none. Wire: none.

## Required

- [A gate host is rented](/quest/m3/ci-host-rented.md) - condition: the maintainer rents the host; then this starts

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the epic whose queue runner this moves
