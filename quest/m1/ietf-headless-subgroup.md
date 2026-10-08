# [S] Rust drops a headless draft 14-17 subgroup

## Goal

A Rust moq-net subscriber on moq-transport drafts 14 to 17 that receives a
subgroup stream starting mid-group (a non-zero first object ID, since those
drafts have no FIRST_OBJECT bit) drops that stream and keeps the
subscription, as `@moq/net` does since #5019, instead of failing the group.

## Plan

First check whether Rust has the bug: a test that opens a subgroup at object
2 on d14 and d16 and expects the subscription to stay up for the next group.
If it fails, apply #5019's rule: on a draft without the bit, a non-zero first
delta takes the same drop path as a cleared FIRST_OBJECT bit on d18. A gap
after an object was delivered still fails that group. If it passes, delete
this quest.
