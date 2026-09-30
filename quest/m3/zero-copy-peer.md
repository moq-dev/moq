# [XS] A physical-NIC peer for the zero-copy sweep

## Goal

A remote peer reachable through a physical NIC is available to sweep
`SENDMSG_ZC` against. Loopback only measures the kernel's forced copy.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

Record the host and link when it exists, so the sweep is reproducible.
