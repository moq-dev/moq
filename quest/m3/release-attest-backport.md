# [XS] Backport release provenance attestations to release

## Goal

The next `moq-cli` and `moq-relay` releases attach archives that pass
`gh attestation verify --repo moq-dev/moq --signer-workflow
moq-dev/moq/.github/workflows/release-binary.yml`, as
[doc/setup/install.md](/doc/setup/install.md) already promises.

## Plan

Found 2026-11-25 (/quest-run in kixel-ai/crabx, from
[kixel-ai/crabx#24](https://github.com/kixel-ai/crabx/pull/24)): `gh attestation
verify` returns 404 for moq-relay 0.17.18's and moq-cli 0.14.18's archives.
[moq-dev/moq#7224](https://github.com/moq-dev/moq/pull/7224) (security audit
SUP-8) added the `attest` job to `release-binary.yml`, and the `id-token` and
`attestations` permissions to its callers `moq-cli.yml` and `moq-relay.yml`,
on `main` only. Tags run `release`'s copies, which lack both, so moq-relay
0.17.18's [release run](https://github.com/moq-dev/moq/actions/runs/45137422511)
had no attest job. crabx runs moq's CLIs from these releases and verifies an
attestation when one exists, so they reach crabx users unverified.

Decided 2026-11-25 (maintainer, /quest-run in kixel-ai/crabx): ✅ backport the
attest job and its callers' permissions to `release` now, as a security backport
that ships with the next release (rejected: wait for
[the cut](/quest/m3/release/cut.md) to bring `main`'s workflows over; also adding
a post-cut verification step to the cut). It changes no shipped code or API.

`main` already has the change, so the backport is one PR against `release`
carrying only #7224's workflow changes, adapted to `release`'s
`release-binary.yml`; this quest's deletion goes with it or in a PR on `main`.
After the next release of each, check its archives with the command above, adding
the archive path (for example, `./<archive>`) and `--source-ref refs/tags/<tag>`.

Public API: none. Wire: none.
