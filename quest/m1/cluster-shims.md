# [XS] Delete the removed cluster flags

## Goal

`cluster::Config` no longer carries the hidden `mesh` and `linger` fields,
and `--cluster-mesh` and `--cluster-linger` are unknown flags like any other.

## Plan

Both fields exist only to refuse their removed settings at startup with a
pointer to the replacement (`deprecated()` in `rs/moq-relay/src/cluster.rs`,
checked there and in `config.rs`). The linger refusal shipped in
moq-relay 0.15.0 and the mesh refusal (#4601) in 0.17.0, so delete the
fields, `deprecated()`, and its checks, per the no-shim rule. Removing public
fields is a break.

Public API: removes two hidden `cluster::Config` fields. Wire: none.
