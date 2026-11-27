# [S] GPU hosts run the nightly video-quality job

## Goal

`nixos2` (RTX 2070, Intel UHD 630) and the maintainer's desktop (Intel Arrow
Lake, AMD RX 9070) each host a self-hosted moq-dev/moq runner. Each runner is
labeled by its GPUs and has the device nodes and driver libraries the
encoders need. `nixos` (RTX 3070 Ti) gets the same through [GPU
CI](/quest/m3/gpu-ci.md).

This is host setup outside this repository. Check it with
`gh api repos/moq-dev/moq/actions/runners`, which should list each runner
online with its labels. Delete this quest once they are listed.

## Plan

- Checked 2026-10-28: neither host has a runner registered. `nixos`'s
  existing runner containers (`moq-ci`, `moq-pro`, `quest-land`) have no
  `/dev/nvidia*` or `/dev/dri`.
- These runners take only the scheduled or manually dispatched
  video-quality job from `main`, never `pull_request`. The workflow has
  read-only permissions. The corpus read token lives in a GitHub environment
  whose deployment branch rule allows only `main`, because a dispatched run
  uses the workflow file from whichever branch is chosen, so a job-level `if`
  cannot protect it.
- Follow `nixos`'s sandboxed runner containers where the host allows it,
  binding in only the GPU device nodes and the host driver's libraries.
- The quality job is nightly, so its runners can stay at home.

## Waiting

- Maintainer: the 2026-11-27 CI policy runs a public repository's async jobs on GitHub-hosted runners and allows no new per-repository self-hosted runner. How do the GPU rows run nightly?
  - A scheduled job on each GPU host that runs over SSH and uploads its rows, with no Actions runner (Recommended) - keeps every GPU row without a repository runner
  - GitHub's NVIDIA GPU runners - billed, and with no Intel or AMD GPU, two hosts' rows go
  - Self-hosted moq-dev/moq runners, as planned - an exception to the policy
