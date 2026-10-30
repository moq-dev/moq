# [S] Workflow and dependency security linting

## Goal

The merge gate fails on a new zizmor finding in `.github/workflows/`, and
Nightly audits the Go and Python dependencies the way it already audits Rust
(`cargo deny`) and Bun.

## Plan

From the 2026-10-27 coverage audit. actionlint already runs; nothing checks
workflow security. An offline zizmor run on 82f7159bb2 gave 230 diagnostics,
182 of them about local-action references, so most are noise to configure
away, not vulnerabilities. Worth fixing on review: explicit permission scopes
for the release App token and narrower workflow-level OIDC permissions.

Decided 2026-10-27 (maintainer):

- zizmor is offline and fast, so it runs in the merge gate. Review every
  finding once: fix it, or record it in zizmor's config with the reason (the
  privileged triggers and caches several workflows already justify in
  comments). Only new findings fail.
- `govulncheck` on the staged Go modules and `pip-audit` on the resolved
  Python environment need advisory databases, so they run nightly beside
  `cargo deny` and alert through `alert.yml`.

Public API: none. Wire: none.
