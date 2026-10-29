# [S] Release artifacts carry provenance

## Goal

Every binary archive a release attaches carries a GitHub build provenance
attestation, and the install docs show how to check one with `gh attestation
verify`, so a user can tell an artifact came from this repository's release
workflow rather than trusting `SHA256SUMS` fetched from the same release.

## Plan

From the 2026-10-27 security audit (SUP-8). `sh/gh/release.sh` writes
`SHA256SUMS` only, and `infra/moq-sh/install.sh` checks hashes downloaded from
the same release, so a compromised release job produces a matching checksum.

Add `actions/attest-build-provenance` (SHA-pinned) with `attestations: write`
to the jobs in `release-binary.yml` that build attached archives. Changing
`moq.sh` to verify signatures was judged not worth it; a docs line is enough.

Public API: none. Wire: none.
