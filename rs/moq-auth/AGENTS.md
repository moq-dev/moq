The authorization contract for Media over QUIC: tokens, auth server HTTP contract, leases, and `moq auth` CLI.

- `Grant` carries session access (`publish`, `subscribe`, `root`, `mounts`), lease bounds (`expires`, `revalidate`), quotas (`rate`), peer trust role (`link`), and stats/billing attribution (`tier`, `label`). Do not add routing cost, transport keep-alive or liveness tuning, or other non-auth controls to `Grant`.
