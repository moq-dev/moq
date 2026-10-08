# [XS] A self-hosted NVIDIA runner is registered

## Goal

A self-hosted GitHub Actions runner is registered for moq-dev/moq on the
maintainer's Linux host (RTX 3070 Ti), with the NVIDIA driver installed. This
is a maintainer action: only someone with admin access to the repository and
the host can do it.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-10-08 `gh api repos/moq-dev/moq/actions/runners` still lists none.
Register it with the dedicated label and hardening that
[GPU CI](/quest/m1/gpu-ci.md) describes.
