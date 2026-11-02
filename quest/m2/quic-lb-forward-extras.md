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

Peer map: each map entry is a QUIC-LB server ID and that relay's forward
socket address (`[listen.lb.forward] bind`), the same pair the TOML file
holds. A `cluster.api.url` peer object carries only `url`, `cost`, `egress`,
`token`, and `upstream` today, so it gains optional fields for the two, with
names to settle in the PR. A peer without both stays out of the forward map,
so a packet naming its server ID is still refused as unknown. Decide whether
the API replaces the TOML file or merges with it when both are set.

Counters: the forwarding module from #7539 (`moq_tokio::quic::driver::forward`)
owns them and the relay exposes them in its stats and `/metrics`:

- forwarded: a short-header packet sent to another server's forward socket;
- unknown server ID: a short header naming a server ID the map lacks, dropped;
- refused: a received forward dropped for an unlisted source, a bad tag, or
  naming a server other than this one.

## Required

- [moq-relay forwards QUIC packets by QUIC-LB server ID](/quest/m2/relay-quic-lb-forward.md) - the forwarding this extends, in [moq-dev/moq#7539](https://github.com/moq-dev/moq/pull/7539)
