# 2026-10-27 security audit: release credential leaks

## Goal

Close the credential leaks from the 2026-10-27 external security audit that
also ship in `release`: tokens reaching logs, and outbound HTTP clients that a
network attacker can redirect or impersonate. Each child fixes `main` and
backports to `release`.

## Plan

An external AI audit at
[651c30fee9](https://github.com/moq-dev/moq/commit/651c30fee9) reported 8
Medium and 28 Low findings. Every claim was re-checked against source before
planning. The completed pre-cut epic's
[triage record](https://github.com/moq-dev/moq/blob/e3bd4b288e01f0c2e7fbe1e6a362d6f3d58057c8/quest/m1/security-audit/README.md)
preserves the maintainer's dropped findings and external owners.

Decided 2026-10-27 (maintainer): these two land in m0 because `release`
carries the same bugs. Each quest owns its own backport PR rather than a
separate `backport-*` quest.

## Required

- [Tokens stay out of logs on release](/quest/m0/security-audit/log-secrets.md) - backport the main-branch credential log redaction to `release`
