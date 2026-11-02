# [S] QUIC-LB forwarding reads the cluster's peers and reports its counts

## Goal

The QUIC-LB forward peer map can come from the `cluster.api.url` peer entries,
not only the watched TOML file. Relay stats count packets forwarded, dropped for
an unknown server ID, and refused.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Follow-ups from [moq-dev/moq#7539](https://github.com/moq-dev/moq/pull/7539).
The maintainer kept the TOML file as the source there (decision 4), with the
cluster API as this follow-up. Use the cluster API only if the deployment's API
already serves the mesh peer list.
