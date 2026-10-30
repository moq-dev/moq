# [XS] Windows receives notice truncated control data

## Goal

moq-sock's Windows receive path detects truncated control data (`MSG_CTRUNC`) and keeps the packet with whatever metadata fit, logging once, as the Unix paths do.

## Plan

Follow-up of [moq-dev/moq#7394](https://github.com/moq-dev/moq/pull/7394), which did this for `recvmmsg`, `recvmsg`, and Apple's `recvmsg_x`. Windows's 128-byte buffer already fits what it uses, so this is defensive.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Match #7394's handling; verify with a cross-target `cargo check` and the platform workflow, since no Windows host runs locally.

Public API: none. Wire: none.

## Required

- [Bound cmsg payload reads](/quest/m2/cmsg-decode-bound.md) - its PR, #7394, adds the Unix handling this mirrors
