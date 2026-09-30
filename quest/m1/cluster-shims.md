# [XS] Delete the removed cluster flags

## Goal

`cluster::Config` no longer carries the hidden `mesh` and `linger` fields,
and `--cluster-mesh` and `--cluster-linger` are unknown flags like any other.

## Plan

Both fields exist only to refuse their removed settings at startup with a
pointer to the replacement (`deprecated()` in `rs/moq-relay/src/cluster.rs`,
checked there and in `config.rs`). The linger refusal shipped in
moq-relay 0.15.0; the mesh refusal (#4601) ships in the next release. Once a
release has carried both, delete the fields, `deprecated()`, and its checks,
per the no-shim rule. Lands on `dev`: removing public fields is a break.

Public API: removes two hidden `cluster::Config` fields. Wire: none.

## Required

- A moq-relay release carrying the `--cluster-mesh` refusal from #4601
