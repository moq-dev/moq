# [XS] NixOS leaf dials its root

## Goal

A relay deployed with `nix/modules/moq-relay.nix` and a cluster root URL
joins that root. Today the module sets `MOQ_CLUSTER_ROOT`, which nothing
reads; the relay reads `MOQ_CLUSTER_CONNECT`
(`rs/moq-relay/src/settings.rs`), and unknown `MOQ_*` variables are silently
ignored, so a NixOS leaf runs standalone.

## Plan

Found 2026-10-22 by the API audit (inherited from release). Rename the
variable in the module, check the module's other `MOQ_*` names against
`settings.rs`, and add a check (a NixOS VM test or an eval-time assertion)
that every variable the module sets is one the relay reads.
