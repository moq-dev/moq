# [S] Smoke tests the released packages

## Goal

The Smoke workflow passes on main. Its clients call only the API of the
latest published release, so a main-only API never breaks it, and every
red cell is fixed at its cause.

## Plan

Decided 2026-10-27 (maintainer), from
[moq-dev/moq#6879](https://github.com/moq-dev/moq/pull/6879): smoke clients
use only released APIs, and a release cut moves them onto new ones. Smoke
has failed on main on every run since at least 2026-10-24 (for example
[run 40591351260](https://github.com/moq-dev/moq/actions/runs/40591351260)).

- Go (`test/smoke/clients/go/smoke.go`) calls `moq.AnnounceRoute`, and C
  (`test/smoke/clients/c/subscribe.c`) calls `moq_connection_state` and
  `moq_media_read`/`MOQ_DECODE_READ_FRAME`. None are on `release`. Revert
  them to the released API.
- Record in the release-cut process (CONTRIBUTING or the release skill,
  wherever the cut's checklist lives) that the cut updates smoke clients to
  the new API.
- Cloudflare fails with "missing fields `end_of_group` and `status`" in
  `moq_transport::serve::Datagram`. Find the cause and fix it.
- Upstream Docker fails because the nix build lacks the `rs/moq-net-tests`
  workspace member. [moq-dev/moq#6931](https://github.com/moq-dev/moq/pull/6931)
  may fix it; confirm after it lands.
- `freshness.sh` already enforces Go `@latest` and the newest `libmoq`;
  keep it.

Public API: none. Wire: none.
